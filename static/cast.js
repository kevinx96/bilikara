/* Host-only DLNA controls. AppState owns the session; this module owns DOM drafts. */
(() => {
  "use strict";
  function create(adapter) {
    const rows = new Map();
    const waiters = new Set();
    const el = id => document.getElementById(id);
    const view = () => adapter.snapshot()?.cast || {};
    const t = key => adapter.translate(key);
    let pending = null;
    let open = false;
    let errorKey = "";
    const busy = () => Boolean(pending || view().busy || ["connecting", "recovering", "discovering"].includes(view().phase));
    const errorText = error => {
      const key = `cast.error.${error?.code || error || "unknown"}`;
      const value = t(key);
      return value === key ? t("cast.error.unknown") : value;
    };
    const text = (node, value) => { if (node && node.textContent !== value) node.textContent = value; };

    function render() {
      const cast = view();
      el("cast-panel").hidden = !open;
      el("cast-open").setAttribute("aria-expanded", String(open));
      el("cast-open").textContent = t("cast.title");
      const blocked = busy() || adapter.physicalBusy();
      for (const button of el("cast-panel").querySelectorAll("button[data-cast-action]")) {
        button.disabled = blocked;
        if (button !== pending) text(button, t(button.dataset.label));
      }
      // Cancellation remains available while the receiver is discovering/connecting.
      el("cast-disconnect").disabled = Boolean(pending === el("cast-disconnect") || !cast.phase || cast.phase === "idle" || cast.phase === "recovering");
      el("cast-discover").disabled ||= Boolean(cast.owns_playback);
      text(el("cast-status"), cast.error_code ? errorText(cast.error_code)
        : t(`cast.phase.${cast.phase || "idle"}`));
      const devices = Array.isArray(cast.renderers) ? cast.renderers : [];
      const keys = new Set(devices.map(device => device.device_key));
      for (const [key, row] of rows) {
        if (!keys.has(key)) { row.remove(); rows.delete(key); }
      }
      for (const device of devices) {
        let row = rows.get(device.device_key);
        if (!row) {
          row = document.createElement("section");
          row.className = "cast-device";
          row.dataset.castDevice = device.device_key;
          const name = document.createElement("h4");
          const connect = document.createElement("button");
          connect.type = "button";
          connect.className = "toolbar-button";
          connect.dataset.castConnect = device.device_key;
          const label = document.createElement("p");
          label.className = "cast-mode-label";
          const modes = document.createElement("div");
          modes.className = "request-subview-tabs cast-modes";
          modes.setAttribute("role", "group");
          for (const mode of ["auto", "file", "live"]) {
            const button = document.createElement("button");
            button.type = "button";
            button.dataset.castMode = mode;
            modes.append(button);
          }
          row.append(name, connect, label, modes);
          rows.set(device.device_key, row);
          el("cast-devices").append(row);
        }
        text(row.querySelector("h4"), device.label);
        const connect = row.querySelector("[data-cast-connect]");
        const selected = cast.owns_playback && cast.selected === device.device_key;
        connect.disabled = blocked || selected;
        if (connect !== pending) text(connect, t(selected ? "cast.connected" : "cast.connect"));
        const deliveryLabel = t(device.preferences_persisted === false ? "cast.deliveryThisRun" : "cast.delivery");
        text(row.querySelector("p"), deliveryLabel);
        row.querySelector("[role=group]").setAttribute("aria-label", deliveryLabel);
        for (const button of row.querySelectorAll("[data-cast-mode]")) {
          const mode = button.dataset.castMode;
          button.disabled = blocked;
          button.setAttribute("aria-pressed", String((cast.devices?.[device.device_key]?.mode || "auto") === mode));
          if (button !== pending) text(button, t(`cast.mode.${mode}`));
        }
      }
      text(el("cast-empty"), t("cast.empty"));
      el("cast-empty").hidden = devices.length > 0;
      if (cast.error_code) {
        const nextKey = `${cast.operation_id}:${cast.error_code}`;
        if (errorKey !== nextKey) { errorKey = nextKey; adapter.error(errorText(cast.error_code)); }
      }
      for (const waiter of waiters) waiter();
      if (pending) {
        pending.disabled = true;
        text(pending.querySelector("[data-presentation-host-play-label]") || (pending.children.length ? null : pending), t("gatcha.adding"));
        pending.setAttribute("aria-label", t("gatcha.adding"));
      }
    }

    function waitUntil(predicate) {
      if (predicate()) return Promise.resolve();
      // Discovery and preparation have different backend deadlines. Keep the
      // activated guard until the authoritative terminal/cancellation snapshot.
      return new Promise(resolve => {
        const check = () => { if (predicate()) { waiters.delete(check); resolve(); } };
        waiters.add(check);
      });
    }
    async function operation(path, payload = {}) {
      const accepted = await adapter.post(`/api/cast/${path}`, payload);
      await adapter.refresh();
      await waitUntil(() => Number(view().operation_id) >= accepted.operation_id
        && !view().busy && !["connecting", "recovering", "discovering"].includes(view().phase));
    }
    async function disconnect() {
      if (view().owns_playback || ["connecting", "recovering", "discovering"].includes(view().phase)) {
        await operation("disconnect");
        await waitUntil(() => !view().owns_playback && view().phase === "idle");
      }
      await adapter.networkPresentation(false);
    }
    async function perform(button, action) {
      if (!button || button.disabled || (pending && button !== el("cast-disconnect"))) return false;
      const labelNode = button.querySelector("[data-presentation-host-play-label]") || (button.children.length ? null : button);
      const disabled = button.disabled, label = labelNode?.textContent;
      const ariaLabel = button.getAttribute("aria-label");
      pending = button;
      adapter.busyChanged?.(true);
      button.disabled = true;
      button.setAttribute("aria-busy", "true");
      text(labelNode, t("gatcha.adding"));
      button.setAttribute("aria-label", t("gatcha.adding"));
      render();
      try { await action(); return true; }
      catch (error) {
        if (error?.code === "cast_revision_conflict") {
          try { await adapter.refresh(); }
          catch { adapter.error(errorText("unknown")); return false; }
        }
        adapter.error(error?.code ? errorText(error) : error?.message || errorText("unknown"));
        return false;
      } finally {
        if (pending === button) pending = null;
        button.disabled = disabled;
        button.removeAttribute("aria-busy");
        text(labelNode, label);
        if (ariaLabel === null) button.removeAttribute("aria-label");
        else button.setAttribute("aria-label", ariaLabel);
        render();
        adapter.busyChanged?.(Boolean(pending));
      }
    }
    async function connect(payload) {
      await adapter.closePhysical();
      if (view().owns_playback) await disconnect();
      await operation("connect", payload);
    }
    async function setDeviceMode(device_key, mode) {
      const post = () => adapter.post("/api/cast/command", {
        command: "cast.set_device_mode", device_key, mode, revision: adapter.snapshot().revision,
      });
      let snapshot;
      try { snapshot = await post(); }
      catch (error) {
        if (error?.code !== "cast_revision_conflict") throw error;
        await adapter.refresh();
        // Recheck the user's target and availability before one bounded retry.
        if (!view().renderers?.some(device => device.device_key === device_key)) {
          throw { code: "cast_device_preferences_unavailable" };
        }
        if (view().busy || ["connecting", "recovering", "discovering"].includes(view().phase)) {
          throw { code: "cast_busy" };
        }
        snapshot = await post();
      }
      adapter.accept(snapshot);
    }
    el("cast-open").addEventListener("click", () => { open = !open; render(); adapter.reposition(); });
    el("cast-discover").addEventListener("click", event => perform(event.currentTarget, () => operation("discover")));
    el("cast-disconnect").addEventListener("click", event => perform(event.currentTarget, disconnect));
    el("cast-purek-connect").addEventListener("click", event => perform(event.currentTarget, () => {
      const purek_qr = el("cast-purek-url").value.trim();
      if (!purek_qr) throw new Error(t("cast.purekRequired"));
      let url;
      try { url = new URL(purek_qr); } catch { /* report the same localized validation error */ }
      // Only reject obvious input mistakes here. Rust owns QR path/query,
      // nested projection URL and receiver-address validation.
      if (new TextEncoder().encode(purek_qr).length > 32768 || /[\x00-\x1f\x7f]/u.test(purek_qr)
        || !url || !["http:", "https:"].includes(url.protocol) || url.hostname !== "wx.chunk.cn") {
        throw { code: "cast_purek_qr_invalid" };
      }
      return connect({ purek_qr, address_confirmed: el("cast-address-confirmed").checked });
    }));
    el("cast-devices").addEventListener("click", event => {
      const button = event.target.closest("button");
      const row = button?.closest("[data-cast-device]");
      if (!row) return;
      perform(button, async () => {
        const device_key = row.dataset.castDevice;
        if (button.dataset.castMode) {
          await setDeviceMode(device_key, button.dataset.castMode);
        } else await connect({ device_key });
      });
    });
    return { render, disconnect, busy, errorText, perform, waitUntil,
      command: payload => operation("command", payload) };
  }
  globalThis.BilikaraCast = { create };
})();
