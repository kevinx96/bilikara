import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import test from 'node:test';
import {chromium} from 'playwright';

const read = file => readFileSync(new URL(`../static/${file}`, import.meta.url), 'utf8');
const html = read('index.html'), dictionary = JSON.parse(read('i18n.json')).languages;
async function open(t, language = 'zh', available = true) {
  const browser = await chromium.launch({headless:true, ...(process.platform === 'win32' ? {channel:'msedge'} : {})});
  t.after(() => browser.close());
  const page = await browser.newPage({viewport:{width:360,height:800}, hasTouch:true});
  const errors = []; page.on('pageerror', e => errors.push(e.message)); t.after(() => assert.deepEqual(errors, []));
  const entry = html.match(/<div class="cast-entry">[\s\S]*?<\/section>\s*<\/div>/)[0];
  const group = html.match(/<section class="android-my-card cast-output-group"[\s\S]*?<\/section>/)[0];
  await page.setContent(`<html data-native-host="true" data-host-platform="android"><style>${['accent-palette.css','styles.css','host-layout.css','request-tabs.css','ui-surfaces.css'].map(read).join('\n')}
    html,body{min-width:0;margin:0;overflow:auto} #android-my-page{display:block;position:static;padding:14px} .cast-entry{display:none}
    </style><section id="android-my-page">${group}</section>${entry}<video></video></html>`);
  for (const file of ['cast-android.js','cast.js','cast-self-test.js']) await page.addScriptTag({content:read(file)});
  await page.evaluate(({language, dictionary, available}) => {
    const calls = [], pending = new Map();
    const state = {cast:{phase:'idle',operation_id:0,connected:false,owns_playback:false,renderers:[]}};
    window.BilikaraAndroidPlatform = {
      captureCastQr(camera) { calls.push({action:camera?'photo':'gallery'}); return new Promise(resolve=>pending.set('capture',resolve)); },
      openCastAuthorization(url) { calls.push({action:'authorize',url}); return new Promise(resolve=>pending.set('authorize',resolve)); },
      shareCastReport() { calls.push({action:'share'}); return new Promise(resolve=>pending.set('share',resolve)); },
    };
    const controller = BilikaraCast.create({snapshot:()=>state, translate:key=>dictionary[language][key]||key,
      physicalBusy:()=>false, closePhysical:async()=>{}, networkPresentation:async()=>{}, reposition:()=>{}, error:()=>{},
      getCapabilities:async()=>({pipeline_available:available}),
      refresh:async()=>controller.render(),
      post:async(path,payload)=>{calls.push({path,payload}); return new Promise((resolve,reject)=>pending.set('post',{resolve,reject}));},
      getReport:async()=>({text:'redacted',diagnostics:{}}),
      saveReport:()=>BilikaraAndroidPlatform.shareCastReport(), shareReport:()=>true,
    });
    window.fixture = {state,calls,controller,pending, media:document.querySelector('video'),
      update(patch) {Object.assign(state.cast,patch);controller.render();},
      resolve(name,value){pending.get(name)(value);},
      rejectPost(code){pending.get('post').reject({code});},
    };
    controller.render();
  }, {language,dictionary,available});
  return page;
}

test('Android output switch cannot open management; add-device survives absent companion', async t => {
  const page = await open(t, 'en', false);
  assert.equal(await page.locator('[data-media-output-toggle]').isDisabled(), true);
  assert.match(await page.locator('[data-media-output-toggle]').getAttribute('aria-label'), /No device connected/);
  await page.locator('[data-media-output-toggle]').dispatchEvent('click');
  assert.equal(await page.locator('.cast-management').evaluate(n=>n.open), false);
  assert.equal(await page.locator('.media-output-pipeline-test').isHidden(), true);
  assert.match(await page.locator('[data-output-status]').textContent(), /unavailable/);
  await page.locator('[data-media-output-add-device]').click();
  assert.equal(await page.locator('.cast-management').evaluate(n=>n.open), true);
  assert.deepEqual(await page.evaluate(()=>fixture.calls), []);
});

test('Android output action is guarded, toggles only output, and clears hidden-context errors', async t => {
  const page = await open(t);
  await page.evaluate(()=>fixture.update({connected:true,selected:'device',phase:'active',owns_playback:true}));
  await page.locator('[data-media-output-toggle]').click();
  assert.equal(await page.locator('[data-media-output-toggle]').getAttribute('aria-busy'), 'true');
  await page.locator('[data-media-output-toggle]').dispatchEvent('click');
  assert.deepEqual(await page.evaluate(()=>fixture.calls), [{path:'/api/cast/output',payload:{enabled:false}}]);
  assert.equal(await page.locator('.cast-management').evaluate(n=>n.open), false);
  await page.evaluate(()=>fixture.rejectPost('cast_pipeline_failed'));
  await page.waitForFunction(()=>!document.querySelector('[data-output-error]').hidden);
  await page.locator('#android-my-page').evaluate(n=>{n.hidden=true;});
  await page.waitForFunction(()=>document.querySelector('[data-output-error]').hidden);
  await page.locator('#android-my-page').evaluate(n=>{n.hidden=false;});
  assert.equal(await page.locator('[data-output-error]').textContent(), '');
});

for (const language of ['zh','en','ja']) test(`Android QR actions and 360px geometry (${language})`, async t => {
  const page = await open(t,language);
  await page.locator('[data-media-output-add-device]').click();
  await page.locator('[data-capture=photo]').click();
  await page.locator('[data-capture=photo]').dispatchEvent('click');
  assert.deepEqual(await page.evaluate(()=>fixture.calls), [{action:'photo'}]);
  assert.equal(await page.locator('[data-capture=photo]').getAttribute('aria-busy'),'true');
  await page.evaluate(()=>fixture.resolve('capture',{cancelled:true}));
  await page.waitForFunction(()=>!document.querySelector('[data-capture=photo]').disabled);
  await page.locator('[data-capture=gallery]').click();
  await page.evaluate(()=>fixture.resolve('capture',{text:'https://wx.chunk.cn/projection/login?room=private'}));
  await page.waitForFunction(()=>!document.querySelector('[data-authorization]').hidden);
  await page.locator('[data-authorization]').click();
  assert.equal(await page.locator('[data-authorization]').getAttribute('aria-busy'),'true');
  await page.evaluate(()=>fixture.resolve('authorize',{opened:true}));
  await page.waitForFunction(()=>!document.querySelector('[data-authorization]').disabled);
  const geometry = await page.evaluate(()=>{
    const n=document.querySelector('.cast-capture'), s=getComputedStyle(n), d=document.querySelector('.cast-management');
    return {padding:s.paddingTop,radius:s.borderRadius,width:d.getBoundingClientRect().width,overflow:d.scrollWidth>d.clientWidth+1,
      heights:[...n.querySelectorAll('button')].map(b=>b.getBoundingClientRect().height),sameMedia:fixture.media===document.querySelector('video')};
  });
  assert.equal(geometry.padding,'20px'); assert.equal(geometry.radius,'14px');
  assert.ok(geometry.width<=360); assert.equal(geometry.overflow,false); assert.ok(geometry.heights.every(h=>h>=44)); assert.equal(geometry.sameMedia,true);
  assert.deepEqual(await page.locator('#cast-description-url').evaluate(n=>[getComputedStyle(n).height,getComputedStyle(n).borderRadius]),['44px','14px']);
  assert.notEqual(await page.locator('[data-authorization]').evaluate(n=>getComputedStyle(n).backgroundColor),'rgba(0, 0, 0, 0)');
  await page.evaluate(() => document.documentElement.style.setProperty('--android-ime-height','300px'));
  assert.ok(await page.locator('.cast-management').evaluate(n=>n.getBoundingClientRect().bottom<=500),'dialog avoids the overlaid keyboard');
  await page.locator('[data-close]').click();
  assert.equal(await page.locator('#cast-purek-url').inputValue(),'');
});

test('Android self-test report shares through the native bridge and stays guarded',async t=>{
  const page=await open(t,'en');
  await page.locator('[data-media-output-add-device]').click();
  await page.locator('[data-cast-compatibility-test]').click();
  await page.evaluate(()=>fixture.update({self_test:{generation:1,report_available:true,running:false}}));
  await page.waitForFunction(()=>!document.querySelector('[data-test-save]').disabled);
  assert.equal(await page.locator('[data-test-save]').textContent(),'Share report');
  await page.locator('[data-test-save]').click();
  await page.locator('[data-test-save]').dispatchEvent('click');
  assert.deepEqual(await page.evaluate(()=>fixture.calls),[{action:'share'}]);
  assert.equal(await page.locator('[data-test-save]').getAttribute('aria-busy'),'true');
  await page.evaluate(()=>fixture.resolve('share',{shared:true}));
  await page.waitForFunction(()=>!document.querySelector('[data-test-save]').hasAttribute('aria-busy'));
});

test('Android pipeline is independent, guarded and clears results when hidden', async t => {
  const page = await open(t, 'en');
  await page.locator('.media-output-pipeline-test').click();
  await page.locator('.media-output-pipeline-test').dispatchEvent('click');
  assert.deepEqual(await page.evaluate(() => fixture.calls), [{path:'/api/cast/pipeline-test',payload:{}}]);
  assert.equal(await page.locator('.cast-management').evaluate(n => n.open), false);
  await page.evaluate(() => {
    fixture.update({operation_id:1,pipeline_running:false,pipeline_result:{ok:true,first_byte:true,sample_b:'verified'}});
    fixture.pending.get('post').resolve({operation_id:1});
  });
  await page.waitForFunction(() => !document.querySelector('.media-output-pipeline-test').hasAttribute('aria-busy'));
  assert.match(await page.locator('[data-output-status]').textContent(), /sample B verified/);
  await page.locator('#android-my-page').evaluate(n => {n.hidden=true;});
  await page.waitForFunction(() => !document.querySelector('[data-output-status]').textContent.includes('verified'));
  assert.deepEqual(await page.evaluate(() => fixture.calls), [{path:'/api/cast/pipeline-test',payload:{}}]);
});

for (const language of ['zh','en','ja']) test(`Android platform errors and network hints (${language})`, async t => {
  const page = await open(t, language);
  await page.locator('[data-media-output-add-device]').click();
  await page.evaluate(() => fixture.update({network_hint:'cellular_default',error_code:'cast_platform_foreground_required'}));
  assert.ok((await page.locator('#cast-status').textContent()).includes(dictionary[language]['cast.network.cellular_default']));
  assert.ok((await page.locator('#cast-status').textContent()).includes(dictionary[language]['cast.error.cast_platform_foreground_required']));
  await page.locator('[data-cast-compatibility-test]').click();
  await page.evaluate(() => fixture.update({error_code:'cast_platform_unavailable'}));
  const message = await page.locator('[data-test-platform]').textContent();
  assert.ok(message.includes(dictionary[language]['cast.error.cast_platform_unavailable']));
  assert.ok(message.includes(dictionary[language]['cast.network.cellular_default']));
});
