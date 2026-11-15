// Real browser/DOM and production Host functions; receiver and window bridge
// are deterministic adapters. This does not certify a physical DLNA receiver.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import nativeTest from 'node:test';
import { chromium } from 'playwright';
function test(name, options, run) {
  return nativeTest(name, options, async t => {
    try { await run(t); } catch (error) { console.error(name, error.stack); throw error; }
  });
}

const read = name => readFileSync(new URL(`../static/${name}`, import.meta.url), 'utf8');
const app = read('app.js');
const section = (start, end) => {
  const from = app.indexOf(start), to = app.indexOf(end, from);
  assert.ok(from >= 0 && to > from, start);
  return app.slice(from, to);
};
const html = read('index.html');
const entry = html.match(/<div class="cast-entry">[\s\S]*?<\/section>\s*<\/div>/)[0];
const dict = JSON.parse(read('i18n.json')).languages;
const styles = ['styles.css', 'request-tabs.css', 'ui-surfaces.css'].map(read).join('\n');

async function open(t, options = {}) {
  const browser = await chromium.launch({ headless: true, args: ['--autoplay-policy=no-user-gesture-required'], ...(process.platform === 'win32' ? { channel: 'msedge' } : {}) });
  t.after(() => browser.close());
  const page = await browser.newPage({ viewport: { width: 700, height: 700 }, hasTouch: true, ...options });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  t.after(() => assert.deepEqual(errors, []));
  await page.setContent(`<style>${styles}
    html,body { margin:0; min-width:0; }
    #presentation-settings-panel { display:block; position:relative; inset:auto; width:min(420px,calc(100vw - 24px)); max-height:calc(100vh - 24px); margin:12px; box-sizing:border-box; overflow:auto; }
    </style><div id="presentation-settings-panel" class="cache-panel presentation-output-panel">${entry}</div>
    <section id="media"><video id="video" loop></video><audio id="audio" loop></audio></section>
    <section id="key-shift-panel"><button>−1</button><input value="0"><button class="cache-advanced-info-button">i</button><span id="host-key-shift-info"></span></section>
    <section id="av-sync-panel"><button>−50</button><input value="0"><button class="cache-advanced-info-button">i</button><span id="host-av-sync-info"></span></section>
    <div id="audio-variant-bar"><button>Original</button></div>`);
  await page.addScriptTag({ content: read('cast.js') });
  await page.evaluate(dict => { window.dictionary = dict; }, dict);
  await page.addScriptTag({ content: `
    let language='zh', pendingRequest=null, calls=[], messages=[], refreshes=0, seekCalls=0;
    const video=document.getElementById('video'), audio=document.getElementById('audio');
    const elements={playerFrame:document.getElementById('media'),keyShiftPanel:document.getElementById('key-shift-panel'),avSyncPanel:document.getElementById('av-sync-panel'),audioVariantBar:document.getElementById('audio-variant-bar')};
    const state={data:{revision:5,playback_generation:3,current_item:{id:'song',display_title:'Song'},cast:{phase:'idle',operation_id:0,owns_playback:false,renderers:[{device_key:'one',label:'Living room',preferences_persisted:false},{device_key:'two',label:'Other room',preferences_persisted:true}],devices:{one:{mode:'auto'}}}},presentationSession:{mode:'singleScreen',phase:'inactive',generation:0},localPlayerMuted:false,localPlayerVolume:0.8,presentationAppliedComposition:'combined',hostPlaybackSession:{video,audio,playbackGeneration:3,readyCommitted:true,logicalPlayIntent:true},localShouldBePlaying:true};
    const t=key=>dictionary[language][key]||key;
    const setTextContent=(node,text)=>{if(node)node.textContent=text;};
    const setAppMessage=text=>messages.push(text);
    const scheduleTopControlPopoverPositionSync=()=>{};
    const renderPresentationOutputControl=()=>{};
    const render=()=>BilikaraCastHost.sync();
    const acceptHostStateSnapshot=snapshot=>{state.data=snapshot;BilikaraCastHost.sync();};
    const fetchState=async()=>{refreshes++;state.data.revision++;BilikaraCastHost.sync();};
    const tauriInvoke=()=>async(command,args)=>{
      calls.push({command,args});
      if(command==='activate_local_presentation')return {mode:'localDualScreen',phase:'active',generation:state.presentationSession.generation+1};
      return {mode:args.active?'networkRenderer':'singleScreen',phase:args.active?'active':'inactive',generation:args.generation+1};
    };
    const presentationDisplayById=()=>({selectable:true});
    const handlePresentationSession=async session=>{state.presentationSession=session;BilikaraCastHost.controller.render();};
    const deactivateLocalPresentation=async()=>{calls.push({command:'closePhysical'});state.presentationSession.phase='recovering';};
    const apiPost=async(path,payload)=>{calls.push({path,payload});return new Promise((resolve,reject)=>{pendingRequest={resolve,reject};});};
    const activeLocalPlayerElements=()=>({video,audio});
    const activePrimaryVideoElement=()=>video;
    const isCurrentHostPlaybackSession=()=>true;
    const clearSplitPlaybackStartupWatchdog=()=>{};
    const clearLocalPlayerSeekState=()=>{};
    const hostPlaybackSessionObservedPlaying=()=>!video.paused;
    const applyStoredVolumeToMountedPlayer=()=>{video.muted=audio.muted=state.localPlayerMuted;};
    const beginSplitPlayerSeek=(v,a,options)=>{seekCalls++;v.currentTime=a.currentTime=options.targetTime;if(options.resumeAfterSeek){v.play().catch(()=>{});a.play().catch(()=>{});}else{v.pause();a.pause();}return true;};
    const renderPresentationHostSurface=()=>{window.displayedPlayback=presentationPlaybackStateModel();};
    ${section('function presentationPlaybackStateModel(', 'function presentationHostAnnouncementSnapshot(')}
    ${section('async function handlePresentationHostControl(', 'function queuePlayerFrameSingleClick(')}
    ${section('function observedHostPlayerStatus(', 'function nextHostPlayerStatusSequence(')}
    ${section('async function handleLocalPlaybackEnded(', 'async function reorderPlaylist(')}
    ${section('async function activateLocalPresentation()', 'async function deactivateLocalPresentation()')}
    ${section('function reconcileHostPlaybackSession(', 'function renderPlayer(')}
    ${section('function initializeCastHost()', 'initializeCastHost();')}
    initializeCastHost();BilikaraCastHost.sync();
    window.fixture={state,calls,messages,video,audio,parents:[video.parentNode,audio.parentNode],
      update(cast){state.data.cast={...state.data.cast,...cast};BilikaraCastHost.sync();},
      resolve(value){pendingRequest.resolve(value);},reject(code){pendingRequest.reject(Object.assign(new Error(code),{code}));},
      counts:()=>({refreshes,seekCalls}),language(value){language=value;document.querySelectorAll('[data-i18n]').forEach(node=>node.textContent=t(node.dataset.i18n));BilikaraCastHost.sync();},
      finishPhysical(){state.presentationSession={mode:'singleScreen',phase:'inactive',generation:2};BilikaraCastHost.controller.render();}};
  ` });
  return page;
}

test('cast rows retain identity, revision conflict refreshes, and pending buttons reject duplicates', { timeout: 60000 }, async t => {
  const page = await open(t);
  await page.locator('#cast-open').click();
  const mode = page.locator('[data-cast-device="one"] [data-cast-mode="live"]');
  await mode.click();
  assert.equal(await mode.getAttribute('aria-busy'), 'true');
  assert.equal(await mode.isDisabled(), true);
  await mode.dispatchEvent('click');
  assert.equal(await page.evaluate(()=>fixture.calls.length), 1);
  assert.deepEqual(await page.evaluate(()=>fixture.calls[0].payload), { command:'cast.set_device_mode',device_key:'one',mode:'live',revision:5 });
  await page.evaluate(()=>{window.savedRow=document.querySelector('[data-cast-device="one"]');fixture.reject('cast_revision_conflict');});
  await page.waitForFunction(()=>fixture.calls.length===2);
  assert.deepEqual(await page.evaluate(()=>fixture.calls[1].payload),{command:'cast.set_device_mode',device_key:'one',mode:'live',revision:6});
  assert.equal(await mode.getAttribute('aria-busy'),'true');
  await page.evaluate(()=>fixture.reject('cast_revision_conflict'));
  await page.waitForFunction(()=>!document.querySelector('[data-cast-mode="live"]').hasAttribute('aria-busy'));
  assert.equal(await page.evaluate(()=>fixture.counts().refreshes), 2);
  assert.match(await page.evaluate(()=>fixture.messages.at(-1)), /状态已更新/u);
  await page.evaluate(()=>fixture.update({renderers:[{device_key:'one',label:'Renamed'},{device_key:'three',label:'New'}],position:12}));
  assert.equal(await page.evaluate(()=>window.savedRow===document.querySelector('[data-cast-device="one"]')),true);
  assert.equal(await page.locator('#cast-open').getAttribute('aria-expanded'),'true');
  assert.equal(await page.locator('[data-cast-device="two"]').count(),0);
  assert.equal(await page.locator('[data-cast-compatibility-test]').isVisible(),false);
  assert.equal(await page.locator('[data-cast-compatibility-mount]').evaluate(node=>node.getBoundingClientRect().height),0);
});

test('mode conflict retries once and rechecks whether the selected device still exists', {timeout:60000}, async t => {
  const page = await open(t);
  await page.locator('#cast-open').click();
  const mode = page.locator('[data-cast-device="one"] [data-cast-mode="live"]');
  await mode.click();
  await page.evaluate(() => fixture.reject('cast_revision_conflict'));
  await page.waitForFunction(() => fixture.calls.length===2);
  await page.evaluate(() => fixture.resolve({...fixture.state.data,devices:undefined,cast:{...fixture.state.data.cast,devices:{one:{mode:'live'}}}}));
  await page.waitForFunction(() => !document.querySelector('[data-cast-mode="live"]').hasAttribute('aria-busy'));
  assert.equal(await mode.getAttribute('aria-pressed'),'true');
  assert.equal(await mode.getAttribute('aria-selected'),null);
  assert.deepEqual(await page.evaluate(() => fixture.messages),[]);
  await page.locator('[data-cast-device="one"] [data-cast-mode="file"]').click();
  await page.evaluate(() => { fixture.state.data.cast.renderers=[]; fixture.reject('cast_revision_conflict'); });
  await page.waitForFunction(() => fixture.messages.length===1);
  assert.equal(await page.evaluate(() => fixture.calls.length),3,'a disappeared device must not be retried');
});

test('discovery waits beyond 45 seconds for the authoritative terminal phase', {timeout:60000}, async t => {
  const page = await open(t);
  await page.clock.install();
  await page.locator('#cast-open').click();
  await page.locator('#cast-discover').click();
  await page.evaluate(() => {fixture.update({phase:'discovering',busy:true,operation_id:1});fixture.resolve({accepted:true,operation_id:1});});
  await page.clock.fastForward(90000);
  assert.equal(await page.locator('#cast-discover').getAttribute('aria-busy'),'true');
  assert.deepEqual(await page.evaluate(() => fixture.messages),[]);
  await page.evaluate(() => fixture.update({phase:'idle',busy:false}));
  await page.waitForFunction(() => !document.getElementById('cast-discover').hasAttribute('aria-busy'));
  assert.equal(await page.locator('#cast-discover').isDisabled(),false);
});

test('Pure K rejects malformed and oversized links locally with the specific translated error', {timeout:60000}, async t => {
  const page = await open(t);
  await page.locator('#cast-open').click();
  assert.equal(await page.locator('#cast-purek-url').getAttribute('maxlength'),'32768');
  const prefix = 'https://wx.chunk.cn/projection/login?place_code=';
  for (const link of ['javascript:alert(1)','https://example.com/projection/login?a=b',prefix+'x'.repeat(32768),prefix+'房'.repeat(11000),prefix+'\u0001']) {
    await page.locator('#cast-purek-url').evaluate((node,value) => {node.value=value;}, link);
    await page.locator('#cast-purek-connect').click();
    await page.waitForFunction(() => !document.getElementById('cast-purek-connect').hasAttribute('aria-busy'));
    assert.equal(await page.evaluate(() => fixture.messages.at(-1)),dict.zh['cast.error.cast_purek_qr_invalid']);
  }
  assert.deepEqual(await page.evaluate(() => fixture.calls),[]);
});

test('cast controls share theme colors, borders and interaction states with ordinary Host controls', {timeout:90000}, async t => {
  const page = await open(t,{hasTouch:false,viewport:{width:1100,height:1100}});
  await page.locator('#cast-open').click();
  await page.evaluate(() => {
    const reference = document.createElement('div');
    reference.className='source-action-row';
    reference.style.cssText='position:fixed;left:500px;top:20px;display:grid;width:300px;gap:12px';
    reference.innerHTML='<button class="toolbar-button" id="reference-secondary">Secondary</button><button class="next-button" id="reference-primary">Primary</button><input type="text" id="reference-input">';
    document.body.append(reference);
    document.querySelector('#cast-disconnect').disabled=false;
  });
  const pairs = [['#cast-discover','#reference-secondary'],['[data-cast-connect="one"]','#reference-secondary'],['#cast-purek-connect','#reference-primary'],['#cast-purek-url','#reference-input']];
  const metrics = node => {const css=getComputedStyle(node);return Object.fromEntries(['backgroundColor','backgroundImage','color','borderTopWidth','borderTopStyle','borderTopColor','height','borderRadius','outlineColor','outlineWidth','boxShadow','opacity'].map(key=>[key,css[key]]));};
  async function observe(selector, interaction) {
    const control = page.locator(selector);
    await page.mouse.move(1090,1090);
    await page.evaluate(() => document.activeElement?.blur());
    await control.evaluate((node,disabled) => {node.disabled=disabled;},interaction==='disabled');
    if (interaction==='hover') await control.hover();
    if (interaction==='focus') await control.focus();
    if (interaction==='pressed') {await control.hover();await page.mouse.down();}
    await control.evaluate(async node => {
      getComputedStyle(node).backgroundColor;
      await Promise.all(node.getAnimations().map(animation=>animation.finished.catch(()=>{})));
    });
    const result=await control.evaluate(metrics);
    if(interaction==='pressed') {await page.mouse.move(1090,1090);await page.mouse.up();}
    await control.evaluate(node=>{node.disabled=false;});
    return result;
  }
  const backgrounds=[];
  for(const theme of ['light','dark','blue']) {
    await page.evaluate(theme => {document.documentElement.dataset.theme=theme;},theme);
    for(const [target,reference] of pairs) {
      for(const interaction of ['normal','hover','focus','pressed','disabled']) {
        assert.deepEqual(await observe(target,interaction),await observe(reference,interaction),`${theme}: ${target} ${interaction}`);
      }
      assert.equal(await page.locator(target).evaluate(node=>node.getBoundingClientRect().height),44);
    }
    backgrounds.push((await observe('#cast-discover','normal')).backgroundColor);
  }
  assert.equal(new Set(backgrounds).size,3,'secondary surfaces must actually follow the theme');
});

test('temporary device preferences are labeled in every language without replacing rows or promising a save', { timeout: 60000 }, async t => {
  const page=await open(t);
  await page.locator('#cast-open').click();
  await page.evaluate(()=>{window.transientRow=document.querySelector('[data-cast-device="one"]');});
  for(const language of ['zh','ja','en']) {
    await page.evaluate(language=>fixture.language(language),language);
    assert.equal(await page.locator('[data-cast-device="one"] .cast-mode-label').textContent(),dict[language]['cast.deliveryThisRun']);
    assert.equal(await page.locator('[data-cast-device="one"] [role="group"]').getAttribute('aria-label'),dict[language]['cast.deliveryThisRun']);
    assert.equal(await page.locator('[data-cast-device="two"] .cast-mode-label').textContent(),dict[language]['cast.delivery']);
    for(const code of ['cast_device_preferences_unavailable','cast_no_current_item']) {
      assert.equal(await page.evaluate(code=>BilikaraCastHost.controller.errorText(code),code),dict[language][`cast.error.${code}`]);
    }
  }
  await page.evaluate(()=>fixture.update({renderers:[{device_key:'one',label:'Renamed',preferences_persisted:true}]}));
  assert.equal(await page.evaluate(()=>transientRow===document.querySelector('[data-cast-device="one"]')),true);
  assert.equal(await page.locator('[data-cast-device="one"] .cast-mode-label').textContent(),dict.en['cast.delivery']);
});

test('program changes retain casting and local nodes, target the new generation, and restore only the latest program', { timeout: 60000 }, async t => {
  const page=await open(t);
  await page.evaluate(()=>fixture.update({phase:'active',owns_playback:true,operation_id:1,position:37,duration:120}));
  await page.waitForFunction(()=>fixture.state.presentationSession.mode==='networkRenderer');
  const nativeCalls=await page.evaluate(()=>fixture.calls.length);
  for(const generation of [4,5]) {
    const actual=await page.evaluate(generation=>{
      fixture.state.data.current_item={id:`song-${generation}`,display_title:`Song ${generation}`};
      fixture.state.data.playback_generation=generation;
      fixture.state.data.playback_program={item_incarnation_id:generation,selected_audio_variant_id:'original'};
      fixture.update({phase:'connecting',busy:true,position:0});
      const result=reconcileHostPlaybackSession(fixture.state.data.current_item);
      const retained=result.kind==='reused'&&result.video===fixture.video&&result.audio===fixture.audio;
      fixture.update({phase:'active',busy:false,position:generation,duration:100});
      return {retained,operation:fixture.state.data.cast.operation_id,mode:fixture.state.presentationSession.mode,
        title:displayedPlayback.title,position:displayedPlayback.currentTimeSeconds,
        held:fixture.video.paused&&fixture.audio.paused&&fixture.video.muted&&fixture.audio.muted};
    },generation);
    assert.deepEqual(actual,{retained:true,operation:1,mode:'networkRenderer',title:`Song ${generation}`,position:generation,held:true});
  }
  assert.equal(await page.evaluate(()=>fixture.calls.length),nativeCalls,'program changes do not disconnect or change presentation ownership');
  assert.equal(await page.evaluate(()=>fixture.counts().seekCalls),0);
  await page.evaluate(()=>{
    const button=document.createElement('button');button.id='new-program-control';document.body.append(button);
    button.addEventListener('click',()=>handlePresentationHostControl('toggle',button));
  });
  await page.locator('#new-program-control').click();
  assert.deepEqual(await page.evaluate(()=>fixture.calls.at(-1).payload),{action:'toggle-pause',playback_generation:5});
  await page.evaluate(()=>{fixture.update({busy:true});fixture.resolve({accepted:true,operation_id:1});});
  assert.equal(await page.locator('#new-program-control').getAttribute('aria-busy'),'true');
  await page.evaluate(()=>fixture.update({busy:false,is_paused:true}));
  await page.waitForFunction(()=>!document.getElementById('new-program-control').hasAttribute('aria-busy'));
  await page.evaluate(()=>fixture.update({phase:'idle',owns_playback:false,resume:{operation_id:2,item_id:'song',playback_generation:3,position:37,is_paused:false}}));
  assert.equal(await page.evaluate(()=>fixture.state.pendingPlaybackRestore||null),null);
  await page.evaluate(()=>fixture.update({resume:{operation_id:2,item_id:'song-5',playback_generation:5,position:12,is_paused:true}}));
  assert.deepEqual(await page.evaluate(()=>fixture.state.pendingPlaybackRestore),{
    itemId:'song-5',itemIncarnationId:5,variantId:'original',currentTime:12,wasPlaying:false,castOperationId:2,
  });
  assert.equal(await page.evaluate(()=>fixture.counts().seekCalls),0,'the old local program must not receive the new position');
});

test('cast connection waits for physical output release and disconnect waits for receiver ownership', { timeout: 60000 }, async t => {
  const page=await open(t);
  await page.locator('#cast-open').click();
  await page.evaluate(()=>{fixture.state.presentationSession={mode:'localDualScreen',phase:'active',generation:1};});
  await page.locator('[data-cast-connect="one"]').click();
  assert.deepEqual(await page.evaluate(()=>fixture.calls.map(c=>c.command||c.path)),['closePhysical']);
  await page.evaluate(()=>fixture.finishPhysical());
  await page.waitForFunction(()=>fixture.calls.some(c=>c.path==='/api/cast/connect'));
  await page.evaluate(()=>fixture.resolve({accepted:true,operation_id:1}));
  assert.equal(await page.locator('[data-cast-connect="one"]').getAttribute('aria-busy'),'true');
  await page.evaluate(()=>fixture.update({phase:'active',owns_playback:true,operation_id:1,selected:'one',position:37,duration:120}));
  await page.waitForFunction(()=>!document.querySelector('[data-cast-connect="one"]').hasAttribute('aria-busy'));
  await page.locator('#cast-disconnect').click();
  await page.evaluate(()=>fixture.resolve({accepted:true,operation_id:2}));
  assert.equal(await page.locator('#cast-disconnect').getAttribute('aria-busy'),'true');
  assert.equal(await page.evaluate(()=>fixture.state.cast?.owns_playback ?? fixture.state.data.cast.owns_playback),true);
  await page.evaluate(()=>fixture.update({phase:'idle',owns_playback:false,operation_id:2}));
  await page.waitForFunction(()=>!document.querySelector('#cast-disconnect').hasAttribute('aria-busy'));
  assert.equal(await page.evaluate(()=>fixture.state.presentationSession.mode),'singleScreen');
  await page.evaluate(()=>fixture.update({phase:'active',owns_playback:true,operation_id:3}));
  await page.waitForFunction(()=>fixture.state.presentationSession.mode==='networkRenderer');
  await page.evaluate(()=>{window.physicalActivation=activateLocalPresentation();});
  await page.evaluate(()=>fixture.resolve({accepted:true,operation_id:4}));
  assert.equal(await page.evaluate(()=>fixture.calls.some(c=>c.command==='activate_local_presentation')),false);
  await page.evaluate(()=>fixture.update({phase:'idle',owns_playback:false,operation_id:4}));
  await page.evaluate(()=>window.physicalActivation);
  assert.equal(await page.evaluate(()=>fixture.state.presentationSession.mode),'localDualScreen');
});

test('cast retains media, mutes and pauses it, shows receiver time, and applies each matching resume once', { timeout: 60000 }, async t => {
  const page=await open(t);
  await page.locator('#cast-open').click();
  // Use the same multi-frame AVC clip as fullscreen_player_browser; the BBDown
  // single-sample fixture is for demux tests, not browser seek settlement.
  const video=readFileSync(new URL('./fixtures/bbdown/video-reordered-start.mp4',import.meta.url)).toString('base64');
  const audio=readFileSync(new URL('./fixtures/bbdown/audio.m4a',import.meta.url)).toString('base64');
  await page.evaluate(({video,audio})=>{fixture.video.src='data:video/mp4;base64,'+video;fixture.audio.src='data:audio/mp4;base64,'+audio;},{video,audio});
  await page.waitForFunction(()=>fixture.video.readyState>=2&&fixture.audio.readyState>=2);
  assert.deepEqual(await page.evaluate(()=>[fixture.video.muted,fixture.audio.muted,fixture.video.volume,fixture.audio.volume]),[false,false,1,1]);
  await page.evaluate(async()=>{await fixture.video.play();await fixture.audio.play();fixture.update({phase:'active',owns_playback:true,operation_id:1,position:37,duration:120,is_paused:false});});
  assert.deepEqual(await page.evaluate(()=>[fixture.video.paused,fixture.audio.paused,fixture.video.muted,fixture.audio.muted]),[true,true,true,true]);
  assert.equal(await page.evaluate(()=>displayedPlayback.currentTimeSeconds),37);
  assert.equal(await page.evaluate(()=>observedHostPlayerStatus('song',fixture.state.hostPlaybackSession,fixture.video)),null);
  assert.equal(await page.evaluate(()=>handleLocalPlaybackEnded('media-ended',fixture.state.hostPlaybackSession)),false);
  await page.evaluate(()=>{window.mediaBindings=0;for(const media of [fixture.video,fixture.audio]){const add=media.addEventListener.bind(media);media.addEventListener=(...args)=>{mediaBindings++;return add(...args);};}});
  assert.equal(await page.locator('#key-shift-panel input').isDisabled(),true);
  assert.equal(await page.locator('#av-sync-panel .cache-advanced-info-button').isDisabled(),false);
  assert.match(await page.locator('#host-key-shift-info').textContent(),/投屏期间/u);
  await page.evaluate(()=>fixture.update({phase:'idle',owns_playback:false,resume:{operation_id:2,item_id:'other',playback_generation:3,position:0.4,is_paused:true}}));
  assert.equal(await page.evaluate(()=>fixture.counts().seekCalls),0,'stale item must not seek');
  await page.evaluate(()=>fixture.update({resume:{operation_id:3,item_id:'song',playback_generation:3,position:0.4,is_paused:true}}));
  await page.waitForFunction(()=>Math.abs(fixture.video.currentTime-0.4)<0.02).catch(async error => {
    console.error('cast resume observation', await page.evaluate(()=>({time:fixture.video.currentTime,audioTime:fixture.audio.currentTime,
      duration:fixture.video.duration,paused:fixture.video.paused,seeking:fixture.video.seeking,counts:fixture.counts(),cast:fixture.state.data.cast})));
    throw error;
  });
  for(let i=0;i<5;i++)await page.evaluate(()=>fixture.update({position:50}));
  assert.equal(await page.evaluate(()=>fixture.counts().seekCalls),1);
  assert.equal(await page.evaluate(()=>mediaBindings),0,'snapshots and resume must not attach another media handler');
  assert.deepEqual(await page.evaluate(()=>[fixture.video===document.getElementById('video'),fixture.audio===document.getElementById('audio'),fixture.parents[0]===fixture.video.parentNode,fixture.parents[1]===fixture.audio.parentNode]),[true,true,true,true]);
  assert.deepEqual(await page.evaluate(()=>[fixture.video.muted,fixture.audio.muted]),[false,false]);
  assert.equal(await page.locator('#key-shift-panel input').isDisabled(),false);
  await page.evaluate(()=>fixture.update({phase:'active',owns_playback:true,operation_id:4}));
  await page.evaluate(()=>fixture.update({phase:'idle',owns_playback:false,resume:{operation_id:5,item_id:'song',playback_generation:2,position:0.7,is_paused:false}}));
  assert.equal(await page.evaluate(()=>fixture.counts().seekCalls),1,'stale generation must not seek');
  await page.evaluate(()=>fixture.update({resume:{operation_id:6,item_id:'song',playback_generation:3,position:0.2,is_paused:false}}));
  await page.waitForFunction(()=>!fixture.video.paused&&!fixture.audio.paused);
  assert.equal(await page.evaluate(()=>fixture.counts().seekCalls),2,'playing receiver resumes local playback');
});

test('cast transport targets the receiver generation and retains its icons while busy', { timeout: 60000 }, async t => {
  const page=await open(t);
  await page.evaluate(()=>{
    const button=document.createElement('button');button.id='transport';
    button.innerHTML='<svg viewBox="0 0 24 24"></svg><span data-presentation-host-play-label>Pause</span>';
    document.body.append(button);window.transportIcon=button.firstElementChild;
    button.addEventListener('click',()=>handlePresentationHostControl('toggle',button));
    fixture.update({phase:'active',owns_playback:true,operation_id:1,position:37,duration:120});
  });
  const button=page.locator('#transport');
  await button.click();
  assert.equal(await button.getAttribute('aria-busy'),'true');
  await button.dispatchEvent('click');
  assert.deepEqual(await page.evaluate(()=>fixture.calls.filter(c=>c.path).map(c=>c.payload)),[{action:'toggle-pause',playback_generation:3}]);
  await page.evaluate(()=>{fixture.update({busy:true});fixture.resolve({accepted:true,operation_id:1});});
  assert.equal(await button.isDisabled(),true);
  await page.evaluate(()=>fixture.update({busy:false,is_paused:true}));
  await page.waitForFunction(()=>!document.getElementById('transport').hasAttribute('aria-busy'));
  assert.equal(await page.evaluate(()=>transportIcon===document.querySelector('#transport svg')),true);
});

test('Pure K link stays a draft across snapshots and cancellation waits for cleanup', { timeout: 60000 }, async t => {
  const page=await open(t);
  await page.locator('#cast-open').click();
  const link='https://wx.chunk.cn/projection/login?place_code=fixture';
  await page.locator('#cast-purek-url').fill(link);
  await page.locator('#cast-address-confirmed').check();
  await page.locator('#cast-purek-connect').click();
  await page.waitForFunction(()=>fixture.calls.some(c=>c.path==='/api/cast/connect'));
  assert.deepEqual(await page.evaluate(()=>fixture.calls.find(c=>c.path).payload),{purek_qr:link,address_confirmed:true});
  await page.evaluate(()=>{fixture.resolve({accepted:true,operation_id:1});fixture.update({phase:'connecting',owns_playback:true,operation_id:1});});
  assert.equal(await page.locator('#cast-purek-url').inputValue(),link);
  await page.locator('#cast-disconnect').click();
  await page.evaluate(()=>fixture.resolve({accepted:true,operation_id:2}));
  assert.equal(await page.locator('#cast-disconnect').getAttribute('aria-busy'),'true');
  await page.evaluate(()=>fixture.update({phase:'idle',owns_playback:false,operation_id:2}));
  await page.waitForFunction(()=>!document.querySelector('#cast-disconnect').hasAttribute('aria-busy')&&!document.querySelector('#cast-purek-connect').hasAttribute('aria-busy'));
});

test('cast controls fit 700/360/320px in Chinese, Japanese and English with shared geometry', { timeout: 60000 }, async t => {
  const page=await open(t);
  await page.locator('#cast-open').click();
  for(const width of [700,360,320])for(const language of ['zh','ja','en']) {
    await page.setViewportSize({width,height:900});
    await page.evaluate(language=>fixture.language(language),language);
    const measured=await page.evaluate(()=>{
      const panel=document.getElementById('cast-panel');
      const mode=panel.querySelector('[data-cast-mode]'),connect=panel.querySelector('[data-cast-connect]');
      const r=mode.getBoundingClientRect(),c=connect.getBoundingClientRect(),style=getComputedStyle(mode);
      return {scroll:panel.scrollWidth,client:panel.clientWidth,modeHeight:r.height,font:style.fontSize,buttonHeight:c.height,
        outside:[...panel.querySelectorAll('input,button,section')].filter(n=>!n.hidden).some(n=>{const r=n.getBoundingClientRect();return r.width&& (r.left<0||r.right>innerWidth);})};
    });
    assert.equal(measured.outside,false,JSON.stringify({width,language,measured}));
    assert.ok(measured.scroll<=measured.client+1,JSON.stringify({width,language,measured}));
    assert.equal(measured.modeHeight,48);assert.equal(measured.font,'16px');assert.ok(measured.buttonHeight>=44);
  }
});
