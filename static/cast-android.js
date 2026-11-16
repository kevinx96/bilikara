/* Android adapts the shared Host cast controller; it owns no receiver state. */
(() => {
  "use strict";
  function create(adapter) {
    const group = document.querySelector('[data-media-output="settings"]');
    if (!group || document.documentElement.dataset.nativeHost !== 'true'
      || document.documentElement.dataset.hostPlatform === 'desktop') return null;
    const t = adapter.translate, view = () => adapter.snapshot()?.cast || {};
    const platform = () => globalThis.BilikaraAndroidPlatform;
    const add = group.querySelector('[data-media-output-add-device]');
    const toggle = group.querySelector('[data-media-output-toggle]');
    const pipeline = group.querySelector('.media-output-pipeline-test');
    const status = group.querySelector('[data-output-status]');
    const error = group.querySelector('[data-output-error]');
    const dialog = document.createElement('dialog');
    dialog.className = 'selection-modal-card cast-self-test cast-management cast-entry';
    dialog.setAttribute('aria-labelledby', 'cast-management-title');
    dialog.innerHTML = `<header class="selection-modal-head"><h2 id="cast-management-title"></h2><button type="button" class="banner-close" data-close><svg viewBox="0 0 24 24" aria-hidden="true"><path d="m6 6 12 12M18 6 6 18" fill="none" stroke="currentColor" stroke-width="2"/></svg></button></header>`;
    document.body.append(dialog);
    const panel = document.getElementById('cast-panel');
    dialog.append(panel);
    const capture = document.createElement('section');
    capture.className = 'cast-capture';
    capture.innerHTML = `<div class="cast-actions"><button type="button" class="toolbar-button" data-capture="photo"></button><button type="button" class="toolbar-button" data-capture="gallery"></button><button type="button" class="toolbar-button" data-capture="paste"></button></div><button type="button" class="next-button" data-authorization hidden></button><p role="status" data-capture-error hidden></p>`;
    panel.insertBefore(capture, document.getElementById('cast-empty'));
    const input = document.getElementById('cast-purek-url');
    const auth = capture.querySelector('[data-authorization]');
    const advanced = document.createElement('details');
    advanced.className = 'cast-advanced';
    const summary = document.createElement('summary'); advanced.append(summary);
    const label = panel.querySelector('label[for="cast-purek-url"]');
    label.before(advanced); advanced.append(label, input);
    const addressLabel = document.createElement('label');
    addressLabel.htmlFor = 'cast-description-url';
    const address = document.createElement('input'); address.id = 'cast-description-url'; address.type = 'text'; address.inputMode = 'url'; address.autocomplete = 'off';
    const manual = document.createElement('button'); manual.type = 'button'; manual.className = 'toolbar-button';
    advanced.append(addressLabel, address, manual);
    let epoch = 0, contextKey = '', errorKey = '', captureError = '', pending = new Set(), available = false, capabilityKnown = false;
    let pipelineResult = '', opener = add;
    function clear() { epoch++; errorKey = ''; captureError = ''; pipelineResult = ''; }
    function code(error) { return String(error?.code || '').startsWith('cast_') ? error.code : 'cast_platform_failed'; }
    function translated(code) { const key = `cast.error.${code}`; return t(key) === key ? t('cast.error.unknown') : t(key); }
    async function action(button, work, captureAction = false) {
      if (button.disabled || pending.size) return;
      const token = epoch, disabled = button.disabled;
      errorKey = ''; captureError = ''; pending.add(button); button.disabled = true; button.setAttribute('aria-busy', 'true'); render();
      try { await work(token); }
      catch (failure) { if (token === epoch) { if (captureAction) captureError = code(failure); else errorKey = code(failure); } }
      finally { pending.delete(button); button.disabled = disabled; button.removeAttribute('aria-busy'); render(); }
    }
    async function capabilities() {
      try { available = (await adapter.getCapabilities()).pipeline_available === true; }
      catch { available = false; }
      capabilityKnown = true; render();
    }
    function openManagement(source = add) {
      opener = source; clear(); adapter.setOpen(true); render();
      if (!dialog.open) dialog.showModal();
    }
    function close() {
      if (pending.size) return;
      clear(); input.value = ''; auth.hidden = true; dialog.close(); adapter.setOpen(false);
      if (opener.getClientRects().length) opener.focus(); render();
    }
    dialog.querySelector('[data-close]').addEventListener('click', close);
    dialog.addEventListener('cancel', event => { event.preventDefault(); close(); });
    add.addEventListener('click', () => openManagement());
    toggle.addEventListener('click', () => {
      if (!view().connected || toggle.disabled) return;
      void action(toggle, async () => {
        await adapter.post('/api/cast/output', {enabled: !view().owns_playback}); await adapter.refresh();
        await adapter.waitUntil(() => !view().busy);
      });
    });
    pipeline.addEventListener('click', () => action(pipeline, async token => {
      const accepted = await adapter.post('/api/cast/pipeline-test', {}); await adapter.refresh();
      await adapter.waitUntil(() => view().operation_id >= accepted.operation_id && !view().pipeline_running);
      const result = view().pipeline_result;
      if (token !== epoch) return;
      if (!result?.ok) throw {code: result?.code || 'cast_pipeline_failed'};
      pipelineResult = result.sample_b === 'verified' ? 'passed' : result.sample_b === 'integrity_failed' ? 'sampleInvalid' : 'sampleMissing';
    }));
    for (const button of capture.querySelectorAll('[data-capture]')) button.addEventListener('click', () => {
      if (button.dataset.capture === 'paste') { advanced.open = true; input.focus(); return; }
      void action(button, async token => {
        const result = await platform().captureCastQr(button.dataset.capture === 'photo');
        if (token !== epoch || result.cancelled) return;
        input.value = result.text || ''; advanced.open = true; render();
      }, true);
    });
    auth.addEventListener('click', () => action(auth, () => platform().openCastAuthorization(input.value.trim()), true));
    input.addEventListener('input', () => { captureError = ''; render(); });
    manual.addEventListener('click', () => action(manual, () => adapter.operation('discover', {description_url: address.value.trim()}), true));
    const observer = new MutationObserver(() => render());
    const my = document.getElementById('android-my-page');
    if (my) observer.observe(my, {attributes:true, attributeFilter:['hidden', 'inert']});
    function render() {
      const cast = view(), hidden = Boolean(my?.hidden || my?.inert);
      const key = `${hidden}:${cast.selected || ''}:${Boolean(cast.connected)}`;
      if (key !== contextKey) { contextKey = key; clear(); }
      const blocked = pending.size > 0 || adapter.busy() || adapter.physicalBusy();
      add.textContent = t(cast.connected ? 'cast.android.manage' : 'cast.android.add');
      toggle.disabled = blocked || !cast.connected;
      toggle.setAttribute('aria-checked', String(Boolean(cast.connected && cast.owns_playback)));
      toggle.setAttribute('aria-label', `${t('cast.title')}${cast.connected ? '' : ` · ${t('cast.android.notConnected')}`}`);
      pipeline.hidden = !available; pipeline.disabled = blocked || Boolean(cast.connected);
      pipeline.textContent = t('cast.android.pipeline');
      status.textContent = pipelineResult ? t(`cast.android.pipeline.${pipelineResult}`) : !available && capabilityKnown ? t('cast.android.pipeline.unavailable') : t(cast.connected ? (cast.owns_playback ? 'cast.android.outputCast' : 'cast.android.outputLocal') : 'cast.android.notConnected');
      error.hidden = !errorKey; error.textContent = errorKey ? translated(errorKey) : '';
      dialog.querySelector('h2').textContent = t('cast.title');
      dialog.querySelector('[data-close]').setAttribute('aria-label', t('cast.test.close'));
      dialog.querySelector('[data-close]').disabled = pending.size > 0;
      summary.textContent = t('cast.android.advanced'); addressLabel.textContent = t('cast.android.description'); manual.textContent = t('cast.discover'); manual.disabled = blocked;
      for (const button of capture.querySelectorAll('[data-capture]')) { button.textContent = t(`cast.android.${button.dataset.capture}`); button.disabled = blocked; }
      let url; try { url = new URL(input.value.trim()); } catch { /* draft */ }
      auth.hidden = !url || !['http:', 'https:'].includes(url.protocol) || url.hostname !== 'wx.chunk.cn' || url.pathname !== '/projection/login';
      auth.textContent = t('cast.android.authorize'); auth.disabled = blocked;
      const captureStatus = capture.querySelector('[data-capture-error]');
      captureStatus.hidden = !captureError; captureStatus.textContent = captureError ? translated(captureError) : '';
      for (const button of pending) { button.disabled = true; if (button !== toggle) button.textContent = t('cast.test.busy'); }
    }
    void capabilities(); render();
    return {render, openManagement};
  }
  globalThis.BilikaraCastAndroid = {create};
})();
