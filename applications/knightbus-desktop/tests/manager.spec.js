import {test,expect} from '@playwright/test';
import AxeBuilder from '@axe-core/playwright';
const PEER = 'a'.repeat(64);
async function fixture(page,role='exporter') {
  await page.addInitScript(({role,peer}) => {
    window.fixture = {role,peer,peers:[peer],online:true,epoch:'9007199254740993',calls:[],devices:[{key:{id:'9007199254740995',generation:'18446744073709551615'},busy:false,shared:false,vendor:1133,product:49685,speed:2,class:0}],sessions:[],deny:false};
    window.__TAURI__ = {core:{invoke:async (command,args) => {
      const f=window.fixture;
      if(command==='defaults') return {endpoint:'/test/control',localEndpoint:'/test/local-control',name:'Test Mac',address:'192.168.1.42'};
      if(command==='quit_service') { f.quitCalls=(f.quitCalls||0)+1; if(f.quitFailure) throw f.quitFailure; f.quit=true; return; }
      if(command==='enable_capture') { f.captureCalls=(f.captureCalls||0)+1; if(f.captureFailure) throw f.captureFailure; f.captureReady=true; f.role='idle'; return '/test/control'; }
      if(command==='start_service') {
        f.starts=(f.starts||0)+1;
        if(f.startFailure) throw f.startFailure;
        f.online=true; return {endpoint:'/test/local-control',started:true};
      }
      if(!f.online) throw 'Unavailable: start the service';
      if(f.versionError) throw 'Version: install matching daemon';
      const req=args.request; f.calls.push(req);
      if(req.kind==='host') {
        const r=req.value;
        if(r.operation==='status') return {host:{value:{fingerprint:peer,certificate:'cafe',peers:f.peers,exporter:role!=='importer',importer:role!=='exporter',discovery:f.discovery!==false,pin_pairing:f.pinSupport!==false,capture_ready:f.captureReady,driver:f.driver,role:f.role,selection:'Selected backend',guidance:'Capture permission is untested.'}}};
        if(r.operation==='pairing_start') { f.pinProgress={state:'waiting',expires_in:120,attempts_left:5}; return {host:{value:{pin:'123456',address:'192.168.1.42:45678',expires_in:120}}}; }
        if(r.operation==='pairing_join') { f.pinProgress={state:'connecting'}; return {text:'done'}; }
        if(r.operation==='pairing_cancel') { f.pinProgress={state:'idle'}; return {text:'done'}; }
        if(r.operation==='pairing_status') { if(f.pinProgress?.state==='paired') f.peers=[...new Set([...f.peers,f.pinProgress.computer.fingerprint])]; return {host:{value:f.pinProgress||{state:'idle'}}}; }
        if(r.operation==='pair') {f.peers=[...new Set([...f.peers,r.fingerprint])];return {text:'done'};}
        if(r.operation==='revoke') {f.peers=f.peers.filter(p=>p!==r.fingerprint);return {text:'done'};}
        if(r.operation==='clear') { f.role='idle'; return {text:'done'}; }
        if(r.operation==='import' || r.operation==='export') { f.role=r.operation==='import'?'importer':'exporter'; return {text:'done'}; }
        if(r.operation==='inspect') return {storage:!!f.storage,speed:f.speed||2,text:'USB Test Controller descriptors'};
        if(r.operation==='discover') return {host:{value:f.nearby||[]}};
        if(r.operation==='advertise' && f.discoveryFailure) throw f.discoveryFailure;
        if(r.operation==='physical') return {host:{value:f.physical || [{id:'9007199254740995',registry:'18446744073709551615',vendor:1133,product:49685,name:'USB Test Controller'}]}};
        return {text:'done'};
      }
      const [op,...fields]=req.value;
      if(op==='info') return {version:2,epoch:f.epoch};
      if(op==='list') {if(f.networkLoss) throw 'Unavailable: network disconnected';return {devices:f.devices};}
      if(op==='sessions') return {sessions:f.sessions.map(s=>s.id)};
      if(op==='status') {if(f.statusFailure) throw 'NotFound: status lost';return {session:f.sessions.find(s=>s.id.serial===fields[1])};}
      if(op==='share' && f.deny) throw 'Denied: exporter refused';
      if(op==='share'||op==='unshare') { const d=f.devices.find(d=>d.key.id===fields[0]); if(d) d.shared=op==='share'; return {text:'done'}; }
      if(op==='permissions') return {peers:f.peers};
      if(op==='inspect') return {storage:!!f.storage,speed:f.speed||2,text:'interface=0 class=03 endpoint=81 Interrupt'};
      if(op==='attach') {f.sessions=[{id:{epoch:f.epoch,serial:'1'},peer:f.peer,state:'Active',devices:[f.devices[0].key],error:null}];return {text:'accepted'};}
      if(op==='detach') {f.sessions[0].state='Closed';return {text:'done'};}
      return {text:'done'};
    }}};
  },{role,peer:PEER});
  await page.goto('/');
  await expect(page.getByRole('button',{name:'Add a computer'})).toBeEnabled();
}
function connectionCode(fingerprint='b'.repeat(64),name='Living room PC') {
  return 'knightbus:1:' + Buffer.from(JSON.stringify({name,fingerprint,certificate:'cafe',address:'192.168.1.20',usbPort:3337,controlPort:3338})).toString('base64');
}
async function openPair(page) { await page.getByRole('button',{name:'Add a computer'}).click(); await page.locator('#pairing-advanced > summary').click(); }
async function share(page) {
  await page.locator('#physical').getByRole('button',{name:'Share',exact:true}).click();
  await page.locator('#share-peer').selectOption(PEER);
  await page.getByRole('button',{name:'Share device',exact:true}).click();
}
async function settings(page,section='advanced') {
  await page.getByRole('tab',{name:'Settings'}).click();
  await page.locator(`#${section} > summary`).click();
}
test('connection codes require independent fingerprint confirmation and never grant devices',async({page})=>{
  await fixture(page); await openPair(page);
  await page.locator('#pair textarea').fill(connectionCode());
  await page.getByRole('button',{name:'Review & pair'}).click();
  await expect(page.locator('#consent-message')).toContainText('b'.repeat(64));
  await page.getByRole('button',{name:'Cancel',exact:true}).click();
  expect(await page.evaluate(()=>fixture.calls.filter(c=>c.value.operation==='pair').length)).toBe(0);
  await page.getByRole('button',{name:'Review & pair'}).click();
  await page.getByRole('button',{name:'Confirm',exact:true}).click();
  await expect(page.locator('#notice-message')).toContainText('Paired here');
  await expect(page.locator('#pairing')).not.toBeVisible();
  await page.getByRole('tab',{name:'Computers',exact:true}).click();
  await expect(page.locator('#paired')).toContainText('Living room PC');
  expect(await page.evaluate(()=>fixture.calls.some(c=>c.kind==='control'&&c.value[0]==='share'))).toBe(false);
});
test('share consent preserves full identifiers and shows denials inside the dialog',async({page})=>{
  await fixture(page); await page.evaluate(()=>fixture.deny=true); await share(page);
  await expect(page.locator('#share-dialog').getByRole('alert')).toContainText('Denied');
  expect(await page.evaluate(()=>fixture.calls.find(c=>c.value[0]==='share').value)).toEqual(['share','9007199254740995','18446744073709551615',PEER]);
});
test('use and disconnect are explicit; unsupported exporter is disabled',async({page})=>{
  await fixture(page,'importer');
  await expect(page.getByRole('button',{name:'Refresh local devices'})).toBeDisabled();
  await page.getByRole('button',{name:'Use device',exact:true}).click();
  await expect(page.locator('#session-list')).toContainText('In use');
  await page.getByRole('button',{name:'Disconnect',exact:true}).click();
  await page.getByRole('button',{name:'Confirm',exact:true}).click();
  await expect(page.locator('#session-list')).toContainText('Disconnected');
});
test('service loss, restart, inventory loss and version mismatch fail visibly',async({page})=>{
  await fixture(page,'importer'); await settings(page);
  await page.evaluate(()=>fixture.online=false); await page.getByRole('button',{name:'Reconnect service'}).click();
  await expect(page.locator('#service-title')).toContainText('disconnected');
  await expect(page.locator('#remote-devices button')).toHaveCount(0);
  await page.evaluate(()=>{fixture.online=true;fixture.epoch='new';fixture.devices=[];});
  await page.getByRole('button',{name:'Reconnect service'}).click();
  await page.getByRole('tab',{name:'Devices',exact:true}).click();
  await expect(page.locator('#remote-devices')).toContainText('No shared devices');
  await page.evaluate(()=>fixture.networkLoss=true); await page.getByRole('button',{name:'Refresh remote devices'}).click();
  await expect(page.locator('#inventory-status')).toContainText('network disconnected');
  await page.getByRole('tab',{name:'Settings'}).click();
  await page.evaluate(()=>fixture.versionError=true); await page.getByRole('button',{name:'Reconnect service'}).click();
  await expect(page.locator('#service-title')).toContainText('version mismatch');
});
test('partial diagnostics are marked incomplete',async({page})=>{
  await fixture(page,'importer'); await page.getByRole('button',{name:'Use device',exact:true}).click();
  await settings(page,'diagnostic-settings');
  await page.evaluate(()=>fixture.statusFailure=true); await page.getByRole('button',{name:'Collect diagnostics'}).click();
  await expect(page.locator('#report')).toHaveValue(/"complete": false/);
  await expect(page.getByRole('alert')).toContainText('NotFound');
});
test('tabs, keyboard cancellation and accessibility',async({page})=>{
  await fixture(page);
  await share(page);
  await page.getByRole('button',{name:'Stop sharing'}).focus(); await page.keyboard.press('Enter');
  await expect(page.getByRole('button',{name:'Cancel',exact:true})).toBeFocused(); await page.keyboard.press('Escape');
  await page.screenshot({path:'../../../lab/m13/ui-redesign/devices.png',fullPage:true});
  await page.getByRole('tab',{name:'Devices',exact:true}).focus(); await page.keyboard.press('ArrowRight');
  await expect(page.getByRole('tab',{name:'Computers',exact:true})).toBeFocused();
  await expect(page.locator('#devices-panel')).toBeHidden();
  for(const tab of ['Devices','Computers','Settings']) {
    await page.getByRole('tab',{name:tab,exact:true}).click();
    expect((await new AxeBuilder({page}).analyze()).violations).toEqual([]);
  }
  await expect(page.locator('#endpoint')).toBeHidden();
  await page.getByRole('tab',{name:'Computers',exact:true}).click(); await openPair(page);
  expect((await new AxeBuilder({page}).analyze()).violations).toEqual([]);
  await page.screenshot({path:'../../../lab/m13/ui-redesign/pairing.png',fullPage:true});
});
test('Escape after earlier confirmation never accepts the next mutation',async({page})=>{
  await fixture(page); await share(page);
  await page.getByRole('button',{name:'Stop sharing'}).click();
  await page.getByRole('button',{name:'Confirm',exact:true}).click();
  await share(page);
  await page.getByRole('button',{name:'Stop sharing'}).click(); await page.keyboard.press('Escape');
  await expect(page.locator('#consent')).not.toBeVisible();
  expect(await page.evaluate(()=>fixture.calls.filter(c=>c.kind==='control'&&c.value[0]==='unshare').length)).toBe(1);
});
test('paired code prefills address and default ports; connect never attaches',async({page})=>{
  await fixture(page,'idle'); await openPair(page);
  await page.locator('#pair textarea').fill(connectionCode());
  await page.getByRole('button',{name:'Review & pair'}).click(); await page.getByRole('button',{name:'Confirm',exact:true}).click();
  await expect(page.locator('#notice-message')).toContainText('Paired here');
  await expect(page.locator('#pairing')).not.toBeVisible();
  await page.locator('#exporter-peer').selectOption('b'.repeat(64));
  await expect(page.locator('#remote-address')).toHaveAttribute('placeholder','192.168.1.20');
  await expect(page.locator('#remote-address')).toHaveValue('');
  await page.getByRole('button',{name:'Connect',exact:true}).click();
  await expect(page.locator('#remote-devices')).toContainText('Use device');
  expect(await page.evaluate(()=>fixture.calls.find(c=>c.value.operation==='import').value)).toEqual({operation:'import',fingerprint:'b'.repeat(64),usb:'192.168.1.20:3337',control:'192.168.1.20:3338'});
  expect(await page.evaluate(()=>fixture.calls.some(c=>c.kind==='control'&&c.value[0]==='attach'))).toBe(false);
});
test('untrusted names are text and malformed codes never reach the daemon',async({page})=>{
  await fixture(page); await openPair(page);
  await page.locator('#pair textarea').fill('knightbus:1:garbage');
  await page.getByRole('button',{name:'Review & pair'}).click();
  await expect(page.locator('#pair-message')).toContainText('invalid');
  expect(await page.evaluate(()=>fixture.calls.some(c=>c.value.operation==='pair'))).toBe(false);
  await page.locator('#pair textarea').fill(connectionCode('b'.repeat(64),'<img src=x onerror=alert(1)>'));
  await page.getByRole('button',{name:'Review & pair'}).click();
  await expect(page.locator('#consent-message img')).toHaveCount(0);
  await page.getByRole('button',{name:'Confirm',exact:true}).click();
  await expect(page.locator('#notice-message')).toContainText('Paired here');
  await expect(page.locator('#pairing')).not.toBeVisible();
  await expect(page.locator('#paired img')).toHaveCount(0);
});
test('physical sharing selects the exact device and only grants the chosen peer',async({page})=>{
  await fixture(page,'idle');
  await page.locator('#physical').getByRole('button',{name:'Share'}).click();
  await page.locator('#share-peer').selectOption(PEER);
  expect(await page.evaluate(()=>fixture.calls.some(c=>c.value.operation==='export'))).toBe(false);
  await page.getByRole('button',{name:'Share device',exact:true}).click();
  await expect(page.locator('#share-dialog')).not.toBeVisible();
  expect(await page.evaluate(()=>fixture.calls.find(c=>c.value.operation==='export').value)).toEqual({operation:'export',registry:'18446744073709551615',usb:'192.168.1.42:3337',control:'192.168.1.42:3338',capture:true,storage_unmounted:false,announce:false});
  expect(await page.evaluate(()=>fixture.calls.filter(c=>c.kind==='control'&&c.value[0]==='share').length)).toBe(1);
});

test('hover and keyboard tooltips replace subtitles',async({page})=>{
  await fixture(page);
  await expect(page.getByRole('heading',{name:'Devices',exact:true})).toBeVisible();
  await expect(page.locator('body')).not.toContainText('Where do you want');
  const help=page.getByRole('button',{name:'About local devices'});
  await help.hover(); await expect(page.getByRole('tooltip')).toContainText('Exporter');
  await page.keyboard.press('Escape'); await expect(page.getByRole('tooltip')).toBeHidden();
  await help.focus(); await expect(page.getByRole('tooltip')).toBeVisible();
  await expect(help).toHaveAttribute('aria-describedby','tooltip');
  await page.keyboard.press('Tab'); await expect(page.getByRole('tooltip')).not.toContainText('Exporter');
});
test('service recovery is explicit and saves the local endpoint without USB mutations',async({page})=>{
  await fixture(page); await page.evaluate(()=>fixture.online=false);
  await settings(page); await page.getByRole('button',{name:'Reconnect service'}).click();
  await expect(page.locator('#service-recovery')).toBeVisible();
  expect(await page.evaluate(()=>fixture.starts||0)).toBe(0);
  await page.getByRole('button',{name:'Start local service',exact:true}).click();
  await expect(page.locator('#service-recovery')).toBeHidden();
  await expect(page.locator('#endpoint')).toHaveValue('/test/local-control');
  expect(await page.evaluate(()=>fixture.starts)).toBe(1);
  expect(await page.evaluate(()=>fixture.calls.some(c=>c.kind==='control'&&['share','attach'].includes(c.value[0])))).toBe(false);
  await page.reload(); await expect(page.locator('#service-recovery')).toBeHidden();
  await expect(page.getByRole('button',{name:'Add a computer'})).toBeEnabled();
  await expect(page.locator('#endpoint')).toHaveValue('/test/local-control');
});
test('failed service startup stays actionable and never claims success',async({page})=>{
  await fixture(page); await page.evaluate(()=>{fixture.online=false;fixture.startFailure='The companion is missing. Reinstall the complete KnightBus package.';});
  await settings(page); await page.getByRole('button',{name:'Reconnect service'}).click();
  await page.getByRole('button',{name:'Start local service',exact:true}).click();
  await expect(page.locator('#service-message')).toContainText('Reinstall');
  await expect(page.locator('#service-title')).toContainText('disconnected');
  await expect(page.getByRole('button',{name:'Start local service',exact:true})).toBeEnabled();
  await page.getByRole('button',{name:'Setup help'}).click();
  await expect(page.locator('#service-guide')).toContainText('knightbusd.exe');
  expect((await new AxeBuilder({page}).analyze()).violations).toEqual([]);
});

test('computer defaults are detected and remain editable without reset controls',async({page})=>{
  await fixture(page);
  await page.getByRole('tab',{name:'Settings'}).click();
  await expect(page.locator('#computer-name')).toHaveValue('');
  await expect(page.locator('#computer-name')).toHaveAttribute('placeholder','Test Mac');
  await expect(page.locator('#local-address')).toHaveValue('');
  await expect(page.locator('#local-address')).toHaveAttribute('placeholder','192.168.1.42');
  await page.locator('#computer-name').fill('Office Mac');
  await page.locator('#local-address').fill('10.0.0.8');
  await page.locator('#computer-name').focus();
  await page.reload();
  await page.getByRole('tab',{name:'Settings'}).click();
  await expect(page.locator('#computer-name')).toHaveValue('Office Mac');
  await expect(page.locator('#local-address')).toHaveValue('10.0.0.8');
  await expect(page.locator('#detect-computer, #clear, #connection')).toHaveCount(0);
  await page.locator('#advanced > summary').click();
  await expect(page.locator('#usb-port')).toHaveValue('3337');
  await expect(page.locator('#control-port')).toHaveValue('3338');
});
test('physical names and direct device actions preserve exact device identity',async({page})=>{
  await fixture(page,'idle');
  await expect(page.locator('#physical')).toContainText('USB Test Controller');
  await expect(page.locator('#physical details')).toHaveCount(0);
  await page.locator('#physical').getByRole('button',{name:'Details',exact:true}).click();
  await expect(page.locator('#inspection')).toContainText('USB Test Controller descriptors');
  expect(await page.evaluate(()=>fixture.calls.find(c=>c.value.operation==='inspect').value)).toEqual({operation:'inspect',registry:'18446744073709551615'});
});

test('background polling never disables buttons or loses a click',async({page})=>{
  await fixture(page,'idle');
  const share=page.locator('#physical').getByRole('button',{name:'Share'});
  await page.evaluate(()=>{
    const original=window.__TAURI__.core.invoke;
    window.__TAURI__.core.invoke=async(command,args)=>{
      if(args?.request?.value?.operation==='status' && !fixture.delayed) {
        fixture.delayed=true;
        await new Promise(resolve=>{fixture.releasePoll=resolve;});
      }
      return original(command,args);
    };
  });
  await page.waitForFunction(()=>typeof fixture.releasePoll==='function');
  await expect(share).toBeEnabled();
  await page.getByRole('button',{name:'Refresh local devices'}).click();
  expect(await page.evaluate(()=>fixture.calls.filter(c=>c.value.operation==='physical').length)).toBe(1);
  await page.evaluate(()=>fixture.releasePoll());
  await expect.poll(()=>page.evaluate(()=>fixture.calls.filter(c=>c.value.operation==='physical').length)).toBe(2);
  await expect(share).toBeEnabled();
});
test('device lists grow with the window and scroll inside their panels',async({page})=>{
  await fixture(page,'importer');
  const dimensions=()=>page.evaluate(()=>({
    local:document.getElementById('local-device-scroll').getBoundingClientRect().height,
    remote:document.getElementById('remote-devices').getBoundingClientRect().height,
    page:document.documentElement.scrollHeight,
    viewport:innerHeight,
  }));
  await page.setViewportSize({width:1120,height:700});
  const small=await dimensions();
  await page.setViewportSize({width:1120,height:1000});
  const large=await dimensions();
  expect(large.local-small.local).toBeCloseTo(300,0);
  expect(large.remote-small.remote).toBeCloseTo(300,0);
  expect(large.page).toBe(large.viewport);
  await page.evaluate(()=>{
    fixture.devices=Array.from({length:30},(_,i)=>({...fixture.devices[0],key:{id:String(i),generation:'1'}}));
  });
  await page.getByRole('button',{name:'Refresh remote devices'}).click();
  expect(await page.locator('#remote-devices').evaluate(node=>node.scrollHeight>node.clientHeight)).toBe(true);
  await page.setViewportSize({width:760,height:600});
  const compact=await dimensions();
  expect(compact.local).toBeGreaterThan(0);
  expect(compact.remote).toBeGreaterThan(0);
  expect(compact.page).toBe(compact.viewport);
});
test('LAN discovery defaults on, respects opt-out and disables unsupported builds',async({page})=>{
  await fixture(page,'idle'); await settings(page);
  await expect(page.locator('#announce')).toBeChecked();
  await expect(page.locator('#listen-address')).toHaveValue('');
  await expect(page.locator('#listen-address')).toHaveAttribute('placeholder','192.168.1.42');
  await page.locator('#announce').uncheck(); await page.reload();
  await settings(page); await expect(page.locator('#announce')).not.toBeChecked();
  await page.locator('#announce').check();
  await page.evaluate(()=>fixture.discovery=false);
  await page.getByRole('button',{name:'Reconnect service'}).click();
  await expect(page.locator('#announce')).toBeDisabled();
  await expect(page.locator('#announce')).not.toBeChecked();
});

test('errors can be dismissed inside the active dialog and shown again',async({page})=>{
  await fixture(page); await page.evaluate(()=>fixture.deny=true); await share(page);
  const dialog=page.locator('#share-dialog');
  await expect(dialog.getByRole('alert')).toContainText('Denied');
  await dialog.getByRole('button',{name:'Close error',exact:true}).focus();
  await page.keyboard.press('Enter');
  await expect(page.getByRole('alert')).toBeHidden();
  await expect(dialog).toBeVisible();
  await page.getByRole('button',{name:'Share device',exact:true}).click();
  await expect(dialog.getByRole('alert')).toContainText('Denied');
  expect((await new AxeBuilder({page}).analyze()).violations).toEqual([]);
});
test('Computers includes pairing and live connections in one tab',async({page})=>{
  await fixture(page,'importer');
  await expect(page.getByRole('tab')).toHaveCount(3);
  await expect(page.locator('.app-subtitle')).toHaveText('Share USB devices between your computers.');
  await page.getByRole('button',{name:'Use device',exact:true}).click();
  await expect(page.getByRole('tab',{name:'Computers',exact:true})).toHaveAttribute('aria-selected','true');
  await expect(page.locator('#paired')).toBeVisible();
  await expect(page.locator('#session-list')).toContainText('In use');
  await expect(page.locator('#connection-count')).toHaveText('1');
  await page.screenshot({path:'../../../lab/m13/ui-polish/computers-merged.png',fullPage:true});
});
for (const role of ['idle','exporter']) test(`local device actions stay beside the name (${role})`,async({page})=>{
  await fixture(page,role);
  const container='#physical';
  for (const width of [1120,760]) {
    await page.setViewportSize({width,height:700});
    const row=page.locator(`${container} .device-card > .row`).first();
    await expect(row.getByRole('button',{name:'Share',exact:true})).toBeVisible();
    await expect(row.getByRole('button',{name:'Details',exact:true})).toBeVisible();
    const title=await row.locator('h4').boundingBox();
    const share=await row.getByRole('button',{name:'Share',exact:true}).boundingBox();
    expect(share.x).toBeGreaterThan(title.x+title.width);
    expect(await row.evaluate(node=>node.scrollWidth<=node.clientWidth)).toBe(true);
  }
});

test('idle importers advertise before pairing and nearby discovery never grants access',async({page})=>{
  await fixture(page,'importer');
  await page.evaluate(()=>{ fixture.role='idle'; fixture.peers=[]; fixture.nearby=[{name:'Hagrid <img src=x onerror=alert(1)>',fingerprint:'b'.repeat(64),usb:'192.168.1.20:3337',control:'192.168.1.20:3338'}]; });
  await expect.poll(()=>page.evaluate(()=>fixture.calls.filter(c=>c.value.operation==='advertise').length)).toBe(1);
  expect(await page.evaluate(()=>fixture.calls.find(c=>c.value.operation==='advertise').value)).toEqual({operation:'advertise',enabled:true,name:'Test Mac',usb:'192.168.1.42:3337',control:'192.168.1.42:3338'});
  await page.getByRole('tab',{name:'Computers'}).click();
  await page.getByRole('button',{name:'Find nearby'}).click();
  await expect(page.locator('#hints')).toContainText('Hagrid <img');
  await expect(page.locator('#hints img')).toHaveCount(0);
  await expect(page.locator('#hints').getByRole('button',{name:'Pair',exact:true})).toHaveCount(1);
  expect(await page.evaluate(()=>fixture.calls.some(c=>['pair','import','export'].includes(c.value.operation)||(c.kind==='control'&&['share','attach'].includes(c.value[0]))))).toBe(false);
  await page.getByRole('button',{name:'Add a computer'}).click();
  await expect(page.locator('#pin-peer')).toContainText('Hagrid <img');
  await expect(page.locator('#pairing')).toBeVisible();
  await expect(page.locator('#pin-progress')).toContainText('Show a PIN');
});

test('discovery opt-out withdraws presence and restart reapplies saved preference',async({page})=>{
  await fixture(page,'importer'); await settings(page);
  await page.locator('#announce').uncheck();
  await expect.poll(()=>page.evaluate(()=>fixture.calls.filter(c=>c.value.operation==='advertise').at(-1)?.value.enabled)).toBe(false);
  await page.reload(); await settings(page);
  await expect(page.locator('#announce')).not.toBeChecked();
  await expect.poll(()=>page.evaluate(()=>fixture.calls.filter(c=>c.value.operation==='advertise').at(-1)?.value.enabled)).toBe(false);
  await page.evaluate(()=>fixture.epoch='9007199254740994');
  await page.getByRole('button',{name:'Reconnect service'}).click();
  await expect.poll(()=>page.evaluate(()=>fixture.calls.filter(c=>c.value.operation==='advertise').length)).toBe(2);
});

test('PIN display is temporary and closing the dialog cancels without sharing',async({page})=>{
  await fixture(page); await page.getByRole('button',{name:'Add a computer'}).click();
  await expect(page.locator('#pairing-advanced')).not.toHaveAttribute('open','');
  await page.getByRole('button',{name:'Show PIN',exact:true}).click();
  await expect(page.locator('#pin-value')).toHaveText('123456');
  await expect(page.locator('#pin-progress')).toContainText('Waiting');
  await page.keyboard.press('Escape');
  await expect(page.locator('#pairing')).not.toBeVisible();
  await expect.poll(()=>page.evaluate(()=>fixture.calls.filter(c=>c.value.operation==='pairing_cancel').length)).toBe(1);
  await expect(page.locator('#pin-value')).toHaveText('');
  expect(await page.evaluate(()=>JSON.stringify(localStorage))).not.toContain('123456');
  expect(await page.evaluate(()=>fixture.calls.some(c=>['pair','import','export'].includes(c.value.operation)||(c.kind==='control'&&['share','attach'].includes(c.value[0]))))).toBe(false);
});
test('entering a PIN pairs both computers and remembers authenticated metadata',async({page})=>{
  await fixture(page,'importer');
  const computer={name:'Hagrid',fingerprint:'b'.repeat(64),usb:'192.168.1.20:3337',control:'192.168.1.20:3338',pairing_port:45678};
  await page.evaluate(computer=>fixture.nearby=[computer],computer);
  await page.getByRole('button',{name:'Add a computer'}).click();
  await page.getByRole('button',{name:'Find computers'}).click();
  await page.locator('#pin-peer').selectOption('b'.repeat(64));
  await page.locator('#pin-input').fill('123456');
  await page.locator('#pin-submit').click();
  await expect(page.locator('#pin-progress')).toHaveText('Pairing…');
  expect(await page.evaluate(()=>fixture.calls.find(c=>c.value.operation==='pairing_join').value)).toEqual({operation:'pairing_join',name:'Test Mac',usb:'192.168.1.42:3337',control:'192.168.1.42:3338',address:'192.168.1.20:45678',pin:'123456'});
  await page.evaluate(computer=>fixture.pinProgress={state:'paired',computer:{...computer,pairing_port:null}},computer);
  await expect(page.locator('#notice-message')).toContainText('Paired with Hagrid');
  await expect(page.locator('#pairing')).not.toBeVisible();
  expect(await page.evaluate(()=>fixture.calls.some(c=>c.value.operation==='pairing_cancel'))).toBe(false);
  await page.locator('#exporter-peer').selectOption('b'.repeat(64));
  await expect(page.locator('#remote-address')).toHaveAttribute('placeholder','192.168.1.20');
  await expect(page.locator('#remote-address')).toHaveValue('');
  expect(await page.evaluate(()=>JSON.stringify(localStorage))).not.toContain('123456');
  expect(await page.evaluate(()=>fixture.calls.some(c=>['pair','import','export'].includes(c.value.operation)||(c.kind==='control'&&['share','attach'].includes(c.value[0]))))).toBe(false);
});
test('expired PINs are cleared and allow a new pairing window',async({page})=>{
  await fixture(page); await page.getByRole('button',{name:'Add a computer'}).click();
  await page.getByRole('button',{name:'Show PIN',exact:true}).click();
  await expect(page.locator('#pin-value')).toBeVisible();
  await page.evaluate(()=>fixture.pinProgress={state:'failed',error:'Deadline'});
  await expect(page.locator('#pin-progress')).toContainText('Show a new PIN');
  await expect(page.locator('#pin-value')).toBeHidden();
  await expect(page.locator('#pin-value')).toHaveText('');
  await expect(page.getByRole('button',{name:'Show PIN',exact:true})).toBeEnabled();
});

test('computer dropdowns default to the first paired computer and preserve a choice', async({page}) => {
  await fixture(page);
  await expect(page.locator('#exporter-peer')).toHaveValue(PEER);
  await page.locator('#physical').getByRole('button',{name:'Share',exact:true}).click();
  await expect(page.locator('#share-peer')).toHaveValue(PEER);
  await page.getByRole('button',{name:'Share device',exact:true}).click();
  await expect(page.locator('#notice')).toContainText('USB Test Controller can now be used');
  await page.getByRole('button',{name:'Close notification'}).click();
  await expect(page.locator('#notice')).toBeHidden();
});
test('physical sharing reports the friendly name before the exporter refresh', async({page}) => {
  await fixture(page,'idle');
  await page.locator('#physical').getByRole('button',{name:'Share',exact:true}).click();
  await page.getByRole('button',{name:'Share device',exact:true}).click();
  await expect(page.locator('#notice')).toContainText('USB Test Controller can now be used');
});
test('full quit requires confirmation and failed cleanup keeps the app open', async({page}) => {
  await fixture(page);
  await page.getByRole('button',{name:'Quit KnightBus',exact:true}).click();
  await page.getByRole('button',{name:'Cancel',exact:true}).click();
  expect(await page.evaluate(()=>fixture.quitCalls||0)).toBe(0);
  await page.evaluate(()=>fixture.quitFailure='Still releasing USB devices');
  await page.getByRole('button',{name:'Quit KnightBus',exact:true}).click();
  await page.getByRole('button',{name:'Confirm',exact:true}).click();
  await expect(page.getByRole('alert')).toContainText('Still releasing');
  expect(await page.evaluate(()=>fixture.quit||false)).toBe(false);
  await page.getByRole('button',{name:'Close error'}).click();
  await page.evaluate(()=>fixture.quitFailure=null);
  await page.getByRole('button',{name:'Quit KnightBus',exact:true}).click();
  await page.getByRole('button',{name:'Confirm',exact:true}).click();
  await expect.poll(()=>page.evaluate(()=>fixture.quit)).toBe(true);
});
test('missing Windows driver is explained before any remote capture', async({page}) => {
  await fixture(page,'importer');
  await page.evaluate(()=>fixture.driver='missing');
  await page.getByRole('button',{name:'Refresh remote devices'}).click();
  await expect(page.locator('#driver-setup')).toContainText('driver is missing');
  await page.getByRole('button',{name:'Use device',exact:true}).click();
  await expect(page.getByRole('alert')).toContainText('driver is missing');
  expect(await page.evaluate(()=>fixture.calls.some(c=>c.kind==='control'&&c.value[0]==='attach'))).toBe(false);
});
test('capture setup precedes sharing and cancellation never grants the device', async({page}) => {
  await fixture(page,'idle');
  await page.evaluate(()=>{fixture.captureReady=false;fixture.captureFailure='Authorization cancelled';});
  await page.getByRole('button',{name:'Refresh local devices'}).click();
  await page.locator('#physical').getByRole('button',{name:'Share',exact:true}).click();
  await page.getByRole('button',{name:'Share device',exact:true}).click();
  await expect(page.getByRole('alert')).toContainText('Authorization cancelled');
  expect(await page.evaluate(()=>fixture.calls.some(c=>c.kind==='control'&&c.value[0]==='share'))).toBe(false);
  await page.evaluate(()=>fixture.captureFailure=null);
  await page.getByRole('button',{name:'Share device',exact:true}).click();
  await expect(page.locator('#notice')).toContainText('USB Test Controller can now be used');
  expect(await page.evaluate(()=>fixture.captureCalls)).toBe(2);
});

test('clearing computer overrides restores defaults for discovery and pairing after reload',async({page})=>{
  await fixture(page);
  await page.getByRole('tab',{name:'Settings'}).click();
  await page.locator('#computer-name').fill('Office');
  await page.locator('#local-address').fill('10.0.0.8');
  await page.locator('#computer-name').fill('');
  await page.locator('#local-address').fill('');
  await page.locator('#computer-name').focus();
  await page.reload();
  await page.getByRole('tab',{name:'Settings'}).click();
  await expect(page.locator('#computer-name')).toHaveValue('');
  await expect(page.locator('#local-address')).toHaveValue('');
  await expect.poll(()=>page.evaluate(()=>fixture.calls.findLast(c=>c.value.operation==='advertise')?.value.name)).toBe('Test Mac');
  expect(await page.evaluate(()=>fixture.calls.findLast(c=>c.value.operation==='advertise').value.usb)).toBe('192.168.1.42:3337');
  await page.getByRole('tab',{name:'Devices',exact:true}).click();
  await page.getByRole('button',{name:'Add a computer'}).click();
  await page.getByRole('button',{name:'Show PIN',exact:true}).click();
  expect(await page.evaluate(()=>fixture.calls.findLast(c=>c.value.operation==='pairing_start').value.name)).toBe('Test Mac');
});
test('remote names and connected empty inventory have useful labels',async({page})=>{
  await fixture(page,'importer');
  await page.evaluate(()=>fixture.devices[0].name='Anker PowerConf C200');
  await page.getByRole('button',{name:'Refresh remote devices'}).click();
  await expect(page.locator('#remote-devices h4')).toHaveText('Anker PowerConf C200');
  await page.evaluate(()=>fixture.devices=[]);
  await page.getByRole('button',{name:'Refresh remote devices'}).click();
  await expect(page.locator('#remote-devices')).toContainText('Share a device on the other computer');
});
test('adding a second shared device preserves active sessions and existing grants',async({page})=>{
  await fixture(page);
  await page.evaluate(()=>{
    fixture.devices[0].shared=true;
    fixture.physical=[{id:'2',registry:'22',vendor:1,product:2,name:'Second device'}];
    fixture.sessions=[{id:{epoch:fixture.epoch,serial:'1'},peer:fixture.peer,state:'Active',devices:[fixture.devices[0].key]}];
  });
  await page.getByRole('button',{name:'Refresh local devices'}).click();
  await page.locator('#physical').getByRole('button',{name:'Share'}).click();
  await page.evaluate(()=>fixture.devices.push({key:{id:'2',generation:'22'},vendor:1,product:2,name:'Second device'}));
  await page.getByRole('button',{name:'Share device',exact:true}).click();
  await expect(page.locator('#share-dialog')).not.toBeVisible();
  expect(await page.evaluate(()=>fixture.calls.some(c=>c.value.operation==='clear'))).toBe(false);
  expect(await page.evaluate(()=>fixture.sessions[0].state)).toBe('Active');
  expect(await page.evaluate(()=>fixture.calls.findLast(c=>c.kind==='control'&&c.value[0]==='share').value)).toEqual(['share','2','22',PEER]);
});
test('stop sharing is directly accessible and requires confirmation',async({page})=>{
  await fixture(page); await share(page);
  const stop=page.locator('#shared-devices').getByRole('button',{name:'Stop sharing',exact:true});
  await expect(stop).toBeVisible();
  await stop.click(); await page.getByRole('button',{name:'Cancel',exact:true}).click();
  expect(await page.evaluate(()=>fixture.calls.some(c=>c.value[0]==='unshare'))).toBe(false);
  const before=await stop.boundingBox();
  await stop.click(); await page.getByRole('button',{name:'Confirm',exact:true}).click();
  await expect(page.locator('#notice-message')).toContainText('no longer shared');
  expect(await page.evaluate(()=>fixture.calls.filter(c=>c.value[0]==='unshare').length)).toBe(1);
  await expect(stop).toHaveCount(0);
  const after=await page.locator('#physical').getByRole('button',{name:'Share',exact:true}).boundingBox();
  expect(after.width).toBe(before.width);
  expect(after.height).toBe(before.height);
  await expect(page.locator('#shared-devices details')).toHaveCount(0);
});
test('connections group all device names under their computer',async({page})=>{
  await fixture(page,'importer');
  await page.evaluate(()=>{
    fixture.devices[0].name='Webcam';
    fixture.devices.push({key:{id:'2',generation:'3'},vendor:1,product:2,name:'Headset'});
    fixture.sessions=fixture.devices.map((d,i)=>({id:{epoch:fixture.epoch,serial:String(i+1)},peer:fixture.peer,state:'Active',devices:[d.key]}));
  });
  await page.getByRole('button',{name:'Refresh remote devices'}).click();
  await page.getByRole('tab',{name:'Computers',exact:true}).click();
  await expect(page.locator('#session-list > .session-card')).toHaveCount(1);
  await expect(page.locator('#session-list .session-devices > li')).toHaveCount(2);
  await expect(page.locator('#session-list')).toContainText('Webcam');
  await expect(page.locator('#session-list')).toContainText('Headset');
});

test('Connect finds an existing paired computer without typing an IP',async({page})=>{
  await fixture(page,'idle');
  await page.evaluate(peer=>fixture.nearby=[{name:'Hagrid',fingerprint:peer,usb:'192.168.88.23:3337',control:'192.168.88.23:3338'}],PEER);
  await expect(page.locator('#remote-address')).not.toBeVisible();
  await page.getByRole('button',{name:'Connect',exact:true}).click();
  await expect.poll(()=>page.evaluate(()=>fixture.calls.find(c=>c.value.operation==='import')?.value.usb)).toBe('192.168.88.23:3337');
  expect(await page.evaluate(()=>fixture.calls.some(c=>c.value.operation==='pair'||c.value[0]==='attach'))).toBe(false);
});
test('paired address updates for the selected computer and survives reload without discovery',async({page})=>{
  await fixture(page,'idle');
  await openPair(page);
  await page.locator('#pair textarea').fill(connectionCode(PEER));
  await page.getByRole('button',{name:'Review & pair'}).click();
  await page.getByRole('button',{name:'Confirm',exact:true}).click();
  await expect(page.locator('#pairing')).not.toBeVisible();
  await expect(page.locator('#remote-address')).toHaveAttribute('placeholder','192.168.1.20');
  await page.reload();
  await page.evaluate(()=>fixture.discovery=false);
  await page.getByRole('button',{name:'Connect',exact:true}).click();
  await expect.poll(()=>page.evaluate(()=>fixture.calls.find(c=>c.value.operation==='import')?.value.control)).toBe('192.168.1.20:3338');
});
test('automatic discovery ignores unpaired addresses and manual overrides are explicit',async({page})=>{
  await fixture(page,'idle');
  await page.evaluate(()=>fixture.nearby=[{name:'Hagrid',fingerprint:'b'.repeat(64),usb:'192.168.88.99:3337',control:'192.168.88.99:3338'}]);
  await page.getByRole('button',{name:'Connect',exact:true}).click();
  await expect(page.locator('#error-message')).toContainText('Cannot find this computer');
  expect(await page.evaluate(()=>fixture.calls.some(c=>c.value.operation==='import'))).toBe(false);
  await page.locator('#remote-address').fill('192.168.88.23');
  await page.getByRole('button',{name:'Connect',exact:true}).click();
  await expect.poll(()=>page.evaluate(()=>fixture.calls.find(c=>c.value.operation==='import')?.value.control)).toBe('192.168.88.23:3338');
});

test('storage consent is visible and required before sharing',async({page})=>{
  await fixture(page,'idle');
  await page.evaluate(()=>{fixture.storage=true;fixture.speed=6;});
  await page.locator('#physical').getByRole('button',{name:'Share',exact:true}).click();
  await expect(page.locator('#storage-confirmation')).toBeVisible();
  await expect(page.locator('#share-message')).toContainText('USB 2');
  await page.getByRole('button',{name:'Share device',exact:true}).click();
  await expect(page.locator('#error-message')).toContainText('Eject all volumes');
  expect(await page.evaluate(()=>fixture.calls.some(c=>c.value.operation==='export'))).toBe(false);
  await page.locator('#storage-unmounted').check();
  await page.getByRole('button',{name:'Share device',exact:true}).click();
  await expect(page.locator('#share-dialog')).not.toBeVisible();
  expect(await page.evaluate(()=>fixture.calls.find(c=>c.value.operation==='export').value.storage_unmounted)).toBe(true);
});
test('SuperSpeed import is explained before capture',async({page})=>{
  await fixture(page,'importer');
  await page.evaluate(()=>fixture.devices[0].speed=6);
  await page.getByRole('button',{name:'Refresh remote devices'}).click();
  await page.getByRole('button',{name:'Use device',exact:true}).click();
  await expect(page.locator('#error-message')).toContainText('USB 2');
  expect(await page.evaluate(()=>fixture.calls.some(c=>c.value[0]==='attach'))).toBe(false);
});
test('paired computers appear once with status beside the name and addresses in details',async({page})=>{
  await fixture(page,'importer');
  await page.evaluate(()=>{fixture.nearby=[{name:'Hagrid',fingerprint:fixture.peer,usb:'192.168.88.23:3337',control:'192.168.88.23:3338'}];});
  await page.getByRole('tab',{name:'Computers',exact:true}).click();
  for (const id of ['paired-refresh','connections-refresh']) {
    await expect(page.locator('#'+id)).toHaveClass('icon-button');
    await expect(page.locator('#'+id)).toHaveText('↻');
  }
  await page.getByRole('button',{name:'Refresh paired computers'}).click();
  await expect(page.locator('#paired h3')).toHaveText('Hagrid');
  await expect(page.locator('#paired .peer-heading .badge')).toHaveText('Nearby');
  await expect(page.locator('#hints')).not.toContainText('Hagrid');
  await expect(page.locator('#paired details p')).not.toBeVisible();
  await page.locator('#paired summary').click();
  await expect(page.locator('#paired details p')).toContainText('192.168.88.23:3337');
  await page.evaluate(()=>fixture.sessions=[{id:{epoch:fixture.epoch,serial:'1'},peer:fixture.peer,state:'Active',devices:[fixture.devices[0].key]}]);
  await page.getByRole('button',{name:'Refresh connections'}).click();
  await expect(page.locator('#paired .badge')).toHaveText('In use');
  await expect(page.locator('#session-list .session-devices > li')).toHaveCount(1);
});
test('retired captures reappear once and use the physical registry for re-sharing',async({page})=>{
  await fixture(page);
  await page.evaluate(()=>{fixture.devices=[];fixture.sessions=[{id:{epoch:fixture.epoch,serial:'1'},peer:fixture.peer,state:'Draining',devices:[{id:'9007199254740995',generation:'1'}]}];});
  await expect(page.locator('#physical .share-toggle')).toBeDisabled();
  await page.evaluate(()=>fixture.sessions[0].state='Closed');
  await expect(page.locator('#physical .share-toggle')).toBeEnabled();
  await page.locator('#physical .share-toggle').click();
  await page.evaluate(()=>fixture.devices=[{key:{id:'9007199254740995',generation:'2'},shared:false,vendor:1133,product:49685,speed:2}]);
  await page.getByRole('button',{name:'Share device',exact:true}).click();
  await expect(page.locator('#share-dialog')).not.toBeVisible();
  expect(await page.evaluate(()=>fixture.calls.find(c=>c.value[0]==='share').value)).toEqual(['share','9007199254740995','2',PEER]);
  await expect(page.locator('#local-device-scroll .device-card')).toHaveCount(1);
});

test('nearby computers can be selected before showing a PIN and resolve their current pairing port',async({page})=>{
  await fixture(page,'importer');
  const computer={name:'Hagrid',fingerprint:'b'.repeat(64),usb:'192.168.1.20:3337',control:'192.168.1.20:3338'};
  await page.evaluate(c=>fixture.nearby=[c],computer);
  await page.getByRole('tab',{name:'Computers',exact:true}).click();
  await page.getByRole('button',{name:'Find nearby',exact:true}).click();
  await expect(page.locator('#hints h3')).toHaveText('Hagrid');
  await page.locator('#hints').getByRole('button',{name:'Pair',exact:true}).click();
  await expect(page.locator('#pin-peer')).toHaveValue(computer.fingerprint);
  await expect(page.locator('#pin-peer option:checked')).toBeEnabled();
  await page.locator('#pin-input').fill('123456');
  await page.locator('#pin-submit').click();
  await expect(page.locator('#pin-progress')).toContainText('Choose Show PIN on Hagrid');
  expect(await page.evaluate(()=>fixture.calls.some(c=>c.value.operation==='pairing_join'))).toBe(false);
  // Opening the remote PIN later changes its port but preserves the selected identity.
  await page.evaluate(()=>fixture.nearby[0].pairing_port=45679);
  await page.locator('#pin-submit').click();
  await expect(page.locator('#pin-progress')).toHaveText('Pairing…');
  expect(await page.evaluate(()=>fixture.calls.find(c=>c.value.operation==='pairing_join').value.address)).toBe('192.168.1.20:45679');
});
test('local devices move between shared and available sections',async({page})=>{
  await fixture(page);
  await expect(page.locator('#physical .device-card')).toHaveCount(1);
  await expect(page.locator('#shared-devices .device-card')).toHaveCount(0);
  await share(page);
  await expect(page.locator('#shared-devices .device-card')).toHaveCount(1);
  await expect(page.locator('#physical .device-card')).toHaveCount(0);
  const shared=await page.locator('#shared-local-title').boundingBox();
  const available=await page.locator('#available-local-title').boundingBox();
  expect(shared.y).toBeLessThan(available.y);
  await page.getByRole('button',{name:'Stop sharing',exact:true}).click();
  await page.getByRole('button',{name:'Confirm',exact:true}).click();
  await expect(page.locator('#shared-devices .device-card')).toHaveCount(0);
  await expect(page.locator('#physical .device-card')).toHaveCount(1);
});
