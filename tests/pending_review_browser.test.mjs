import assert from 'node:assert/strict';
import { existsSync, statSync } from 'node:fs';
import path from 'node:path';
import test from 'node:test';
import { chromium } from 'playwright';

const staticRoot = path.resolve(import.meta.dirname, '../static');
const items = Array.from({ length: 12 }, (_, index) => ({
  bvid: `BV${String(index).padStart(10, '0')}`,
  title: `Pending ${index} — 日本語 / 中文 / English`,
  url: `https://www.bilibili.com/video/BV${String(index).padStart(10, '0')}`,
}));

async function open(t, width, language) {
  const browser = await chromium.launch({ headless: true,
    channel: process.env.BILIKARA_TEST_BROWSER_CHANNEL || (process.platform === 'win32' ? 'msedge' : undefined) });
  let finish;
  t.after(async () => {
    try { if (finish) await finish({}, 503); }
    finally { await browser.close(); }
  });
  const page = await browser.newPage({ viewport: { width, height: 900 }, reducedMotion: 'reduce' });
  page.setDefaultTimeout(7000);
  const errors = [], calls = [];
  page.on('pageerror', error => errors.push(error.message));
  t.after(() => assert.deepEqual(errors, []));
  const fulfill = (route, data, status = 200) => route.fulfill({ status, contentType: 'application/json',
    body: JSON.stringify(status === 200 ? { ok: true, data } : { ok: false, error: 'Offline review failure' }) });
  // The existing offline review fixture's seam, with the complete production
  // scripts, result cards and event bindings. No native Host build or D1 traffic.
  await page.route('**/*', async route => {
    const url = new URL(route.request().url());
    if (url.origin !== 'http://review.test') return route.abort();
    if (url.pathname.startsWith('/api/admin-review/')) {
      calls.push({ url: url.pathname, body: route.request().postDataJSON() });
      if (url.pathname.endsWith('/pending')) return fulfill(route, { items, total_pending: 12, export_count: 100 });
      await new Promise(resolve => {
        finish = async (data, status) => { finish = null; await fulfill(route, data, status); resolve(); };
      });
      return;
    }
    // Unrelated bootstrap reads are unavailable in this offline fixture.
    if (url.pathname.startsWith('/api/')) return route.abort();
    const filename = path.resolve(staticRoot, `.${url.pathname === '/' ? '/index.html' : url.pathname}`);
    if (!filename.startsWith(staticRoot + path.sep) || !existsSync(filename) || !statSync(filename).isFile()) return route.abort();
    return route.fulfill({ path: filename });
  });
  await page.goto('http://review.test/');
  await page.waitForFunction(() => typeof state !== 'undefined' && Object.keys(state.translations || {}).length);
  await page.evaluate(language => {
    state.language = language;
    setDeveloperMode(true);
    state.bilikaraSecret = 'offline-review-secret';
    state.activeHostWorkspace = 'request';
    renderHostWorkspaceSelection();
    activateRequestSubview('discover');
  }, language);
  await page.locator('#catalog-advanced-menu > summary').click();
  await page.locator('[data-catalog-tool="review"]').click();
  await page.waitForFunction(() => state.pendingReviewLoaded && !state.pendingReviewLoading);
  assert.equal(await page.locator('[data-pending-review-results] > .search-result-item').count(), items.length);
  return { page, calls, finish: (data, status = 200) => finish(data, status) };
}

async function scrollToActions(page) {
  const region = page.locator('#catalog-advanced-view');
  const box = await region.boundingBox();
  assert(box && box.height > 0);
  // Wheel input cannot scroll overflow:hidden, whereas locator.click() and
  // scrollIntoView() can do so programmatically and would hide this regression.
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.wheel(0, 10000);
  const scroll = await region.evaluate(node => ({ height: node.clientHeight, content: node.scrollHeight }));
  if (scroll.content > scroll.height) {
    await page.waitForFunction(() => {
      const node = document.getElementById('catalog-advanced-view');
      return node.scrollTop + node.clientHeight >= node.scrollHeight - 2;
    }, null, { timeout: 1500 });
  }
}

async function center(page, selector) {
  const point = await page.locator(selector).evaluate(button => {
    const rect = button.getBoundingClientRect();
    const x = rect.x + rect.width / 2, y = rect.y + rect.height / 2;
    const hit = document.elementFromPoint(x, y);
    return { x, y, width: rect.width, height: rect.height, disabled: button.disabled,
      reachable: hit === button || button.contains(hit), hit: hit?.outerHTML.slice(0, 200) };
  });
  assert(point.width > 0 && point.height > 0, JSON.stringify(point));
  assert.equal(point.disabled, false, selector);
  assert.equal(point.reachable, true, `${selector}: ${JSON.stringify(point)}`);
  return point;
}

for (const [width, language] of [[1440, 'zh'], [700, 'en'], [700, 'ja']]) {
  for (const expanded of [false, true]) {
    test(`Host review actions receive pointer input at ${width}px / ${language} / expanded=${expanded}`, { timeout: 30000 }, async t => {
      const fixture = await open(t, width, language);
      const { page, calls } = fixture;
      if (expanded) await page.locator('#request-workspace-expand').click();
      await page.evaluate(() => Promise.all(document.getAnimations()
        .filter(animation => animation.effect?.getTiming().iterations !== Infinity)
        .map(animation => animation.finished.catch(() => {}))));
      await scrollToActions(page);
      await center(page, '[data-pending-review-approve]');
      const reject = await center(page, '[data-pending-review-reject]');
      await page.mouse.click(reject.x, reject.y);
      await page.locator('#developer-tag-reset-modal').waitFor({ state: 'visible' });
      assert.equal(await page.evaluate(() => state.developerTagResetAction), 'reject-page');
      assert.deepEqual(await page.evaluate(() => state.developerTagResetItem.items.map(item => item.bvid)), items.map(item => item.bvid));
      assert.equal(calls.filter(call => call.url.endsWith('/reject')).length, 0, 'Opening the dialog does not reject');
      await page.locator('#developer-tag-reset-cancel').click();
      await page.locator('#developer-tag-reset-modal').waitFor({ state: 'hidden' });

      const rejectAgain = await center(page, '[data-pending-review-reject]');
      await page.mouse.click(rejectAgain.x, rejectAgain.y);
      const rejection = page.waitForRequest('**/api/admin-review/reject');
      await page.locator('#developer-tag-reset-confirm').click();
      await rejection;
      const confirm = page.locator('#developer-tag-reset-confirm');
      assert.equal(await confirm.isDisabled(), true);
      assert.equal(await confirm.getAttribute('aria-busy'), 'true');
      assert.equal(await confirm.textContent(), await page.evaluate(() => t('search.reviewApproving')));
      await confirm.dispatchEvent('click');
      assert.equal(calls.filter(call => call.url.endsWith('/reject')).length, 1);
      assert.deepEqual(calls.at(-1).body, {
        bvid: items[0].bvid, record: { bvid: items[0].bvid, title: items[0].title, url: items[0].url },
        rejected_by: 'VZRXS', BILIKARA_ADMIN_SECRET: 'offline-review-secret',
      });
      await fixture.finish({}, 503);
      await page.waitForFunction(() => !state.developerTagResetSaving);
      assert.equal(await confirm.isDisabled(), false);
      assert.equal(await confirm.getAttribute('aria-busy'), null);
      assert.equal(await page.locator('#developer-tag-reset-error').isVisible(), true);
      await page.locator('#developer-tag-reset-cancel').click();

      for (const fail of [true, false]) {
        const approve = await center(page, '[data-pending-review-approve]');
        const approval = page.waitForRequest('**/api/admin-review/approve');
        await page.mouse.click(approve.x, approve.y);
        await approval;
        const button = page.locator('[data-pending-review-approve]');
        assert.equal(await button.isDisabled(), true);
        assert.equal(await button.getAttribute('aria-busy'), 'true');
        assert.equal(await button.textContent(), await page.evaluate(() => t('search.reviewApproving')));
        assert.equal(await page.locator('[data-pending-review-reject]').isDisabled(), true);
        const before = calls.length;
        await button.dispatchEvent('click');
        assert.equal(calls.length, before, 'Pending approval rejects duplicate activation');
        assert.deepEqual(calls.at(-1).body, { bvids: items.map(item => item.bvid), limit: 20, BILIKARA_ADMIN_SECRET: 'offline-review-secret' });
        await fixture.finish({ approved: items.length, items: [], total_pending: 0, export_count: 100 }, fail ? 503 : 200);
        await page.waitForFunction(() => !state.pendingReviewApproving);
        assert.equal(await button.getAttribute('aria-busy'), null);
        assert.equal(await button.textContent(), await page.evaluate(() => t('search.reviewApprove')));
        assert.equal(await button.isDisabled(), !fail, 'Failure restores retry; empty results disable approval');
        assert.equal(await page.locator('[data-pending-review-reject]').isDisabled(), !fail);
      }
      assert.equal(calls.filter(call => call.url.endsWith('/approve')).length, 2);
    });
  }
}
