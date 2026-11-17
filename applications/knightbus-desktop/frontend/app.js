import './tooltips.js';
import { DEFAULT_USB_PORT, DEFAULT_CONTROL_PORT, MAX_NAME_LENGTH, encodeCard, decodeCard, destination } from './connection.js';

const REFRESH_MS = 3000;
const MAX_ACTIONS = 40;
const MAX_CONTACTS = 128;
const SHORT_FINGERPRINT_LENGTH = 8;
const USB_ID_HEX_WIDTH = 4;
const USB_SUPER_SPEED = 5;
const USB_SUPER_PLUS_SPEED = 6;
const JSON_INDENT = 2;
const PROFILE_KEY = 'knightbus.contacts.v1.';
const PREFERENCES_KEY = 'knightbus.preferences.v1';
const $ = id => document.getElementById(id);
const invoke = (command, args) => window.__TAURI__.core.invoke(command, args);
let connected = false, busy = false, status = null, inventory = [], sessions = [], epoch = null;
let shareInspection = null;
let physical = [], shareTarget = null, connectedPeer = null, connectedDestination = null, profileIdentity = null;
let contacts = {}, renderedDevices = '', renderedSessions = '', renderedPeers = '';
const actions = [];
let serviceFailure = '';
let startupFailure = '';
let localEndpoint = '';
let backgroundRefresh = null;
let discoveryPreference = true;
let discoveryApplied = '';
let discoveryFailure = '';
const effective = id => $(id).value.trim() || $(id).placeholder;
let serviceStarting = false;
let errorFocus = null;
let nearbyComputers = [];
let discoveryChecked = false, renderedNearby = null;
let pairingActive = false;
let pairingCancelQueued = false;
let pinTarget = null;
const PIN_DIGITS = 6;

function readSaved(key) {
  try { return JSON.parse(localStorage.getItem(key)) || {}; } catch { return {}; }
}
function save(key, value) {
  try { localStorage.setItem(key, JSON.stringify(value)); }
  catch { notice('Your preferences could not be saved. Pairing is still managed by the service.'); }
}
function notice(value) {
  const modal = [...document.querySelectorAll('dialog[open]')].at(-1);
  (modal || $('alerts')).append($('notice'));
  $('notice-message').textContent = value; $('notice').hidden = false;
}
$('notice-close').addEventListener('click', () => { $('notice').hidden = true; });
function error(value) {
  if (!$('error').contains(document.activeElement)) errorFocus = document.activeElement;
  const modal = [...document.querySelectorAll('dialog[open]')].at(-1);
  // Keep the close control reachable when a modal triggered the error.
  if (modal) modal.append($('error')); else $('alerts').prepend($('error'));
  $('error-message').textContent = String(value); $('error').hidden = false;
}
$('error-close').addEventListener('click', () => {
  const modal = $('error').closest('dialog');
  $('error').hidden = true;
  if (errorFocus?.isConnected && !errorFocus.disabled && errorFocus.getClientRects().length && (!modal || modal.contains(errorFocus))) errorFocus.focus();
  else (modal?.querySelector('[data-close]') || document.querySelector('[role=tab][aria-selected=true]')).focus();
});
function log(value) {
  actions.unshift(`${new Date().toLocaleTimeString()} ${value}`);
  actions.length = Math.min(actions.length, MAX_ACTIONS);
  $('activity').textContent = actions.join('\n');
}
function controls() {
  $('announce').disabled = busy || !connected || !status?.discovery;
  $('announce').checked = discoveryPreference && !!status?.discovery;
  $('start-service').disabled = busy || serviceStarting;
  $('retry-service').disabled = busy || serviceStarting;
  for (const b of document.querySelectorAll('[data-connected]')) b.disabled = busy || !connected;
  for (const b of document.querySelectorAll('[data-exporter]')) b.disabled = busy || !connected || !status?.exporter;
  for (const b of document.querySelectorAll('[data-importer]')) b.disabled = busy || !connected || !status?.importer;
  $('browse').disabled = busy || !connected || !status?.discovery;
  for (const id of ['pin-start', 'pin-submit', 'pin-peer', 'pin-endpoint', 'pin-input']) $(id).disabled = busy || !connected || !status?.pin_pairing || pairingActive;
  $('pin-find').disabled = busy || !connected || !status?.discovery || pairingActive;
  for (const b of document.querySelectorAll('#pairing-advanced [data-connected]')) b.disabled = busy || !connected || pairingActive;
  $('pin-cancel').hidden = !pairingActive;
  for (const b of document.querySelectorAll('[data-blocked]')) b.disabled = true;

}
function text(tag, value, parent, className = '') {
  const node = document.createElement(tag);
  node.textContent = value; node.className = className; parent.append(node); return node;
}
function box(parent, className) { return text('div', '', parent, className); }
function button(label, action, parent, capability = 'connected') {
  const node = text('button', label, parent, 'secondary');
  node.type = 'button'; node.dataset[capability] = ''; node.addEventListener('click', action); return node;
}
function details(parent, title) {
  const node = document.createElement('details'); parent.append(node); text('summary', title, node); return node;
}
function deviceName(d) {
  const local = status?.role === 'exporter' && d.key ? physical.find(p => p.id === d.key.id) : null;
  const name = d.name || local?.name;
  return typeof name === 'string' && name.trim() ? name.trim() : `USB ${d.vendor.toString(16).padStart(USB_ID_HEX_WIDTH, '0')}:${d.product.toString(16).padStart(USB_ID_HEX_WIDTH, '0')}`;
}
function deviceActions(parent, primary, primaryAction, inspectAction, capability) {
  const actions = box(parent, 'device-actions');
  const main = button(primary, primaryAction, actions, capability); main.className = 'share-toggle';
  const detail = button('Details', inspectAction, actions, capability);
  detail.dataset.tooltip = 'Read device descriptors and identifiers.';
  return actions;
}
function peerName(peer) {
  const name = contacts[peer]?.name || nearbyComputers.find(c => c.fingerprint === peer)?.name;
  return typeof name === 'string' && name.trim() ? name.slice(0, MAX_NAME_LENGTH) : `Computer ${peer.slice(0, SHORT_FINGERPRINT_LENGTH)}`;
}
function selectTab(id, focus = false) {
  for (const tab of document.querySelectorAll('[role=tab]')) {
    const selected = tab.id === id;
    tab.setAttribute('aria-selected', selected); tab.tabIndex = selected ? 0 : -1;
    $(tab.getAttribute('aria-controls')).hidden = !selected;
    if (selected && focus) tab.focus();
  }
}
for (const tab of document.querySelectorAll('[role=tab]')) {
  tab.addEventListener('click', () => selectTab(tab.id));
  tab.addEventListener('keydown', event => {
    const tabs = [...document.querySelectorAll('[role=tab]')];
    let next = tabs.indexOf(tab);
    if (event.key === 'ArrowRight') next = (next + 1) % tabs.length;
    else if (event.key === 'ArrowLeft') next = (next + tabs.length - 1) % tabs.length;
    else if (event.key === 'Home') next = 0;
    else if (event.key === 'End') next = tabs.length - 1;
    else return;
    event.preventDefault(); selectTab(tabs[next].id, true);
  });
}
async function request(value) {
  const started = performance.now();
  const result = await invoke('request', { endpoint: $('endpoint').value.trim(), request: value });
  $('quality').textContent = `Local service response: ${Math.round(performance.now() - started)} ms. This does not measure USB or network latency.`;
  return result;
}
const host = (operation, fields = {}) => request({ kind: 'host', value: { operation, ...fields } });
const control = (...args) => request({ kind: 'control', value: args });
function confirmAction(message) {
  $('consent-message').textContent = message;
  // Escape must never reuse an earlier confirmation.
  $('consent').returnValue = 'cancel';
  return new Promise(resolve => {
    $('consent').addEventListener('close', () => resolve($('consent').returnValue === 'confirm'), { once: true });
    $('consent').showModal();
  });
}
async function act(label, fn, confirmation) {
  if (busy) return;
  busy = true; controls();
  try {
    // Finish an in-flight read before mutating; background reads never disable controls.
    await backgroundRefresh;
    if (confirmation && !await confirmAction(confirmation)) return;
    $('error').hidden = true;
    await fn(); log(label); await refresh();
  } catch (e) {
    error(e); log(`${label} failed: ${e}`);
  } finally { busy = false; controls(); if (pairingCancelQueued) { pairingCancelQueued = false; void cancelPairing(); } }
}
function inspect(fn) {
  return act('Device details opened', async () => {
    $('inspection').textContent = await fn(); $('inspection-dialog').showModal();
  });
}
function drawPeers() {
  const signature = JSON.stringify([status.peers, contacts, nearbyComputers, sessions]);
  if (signature === renderedPeers) return;
  renderedPeers = signature; $('paired').replaceChildren();
  for (const id of ['exporter-peer', 'share-peer']) {
    const selected = $(id).value;
    $(id).replaceChildren(new Option('Choose a paired computer', ''));
    for (const peer of status.peers) $(id).append(new Option(peerName(peer), peer));
    $(id).value = status.peers.includes(selected) ? selected : (status.peers[0] || '');
    if (id === 'exporter-peer') fillRemoteAddress($(id).value !== selected);
  }
  if (!status.peers.length) text('p', 'No paired computers.', $('paired'), 'empty');
  for (const peer of status.peers) {
    const row = box($('paired'), 'peer-card');
    const heading = box(row, 'peer-heading'); text('h3', peerName(peer), heading);
    const active = sessions.filter(s => s.peer === peer && s.state !== 'Closed');
    const nearby = nearbyComputers.find(c => c.fingerprint === peer);
    const state = active.find(s => s.state === 'Active') || active[0];
    text('span', state ? SESSION_LABELS[state.state] : nearby ? 'Nearby' : 'Paired', heading, 'badge');
    const more = details(row, 'Identity & pairing');
    const contact = contacts[peer];
    if (nearby) text('p', `USB ${nearby.usb} · control ${nearby.control}`, more);
    else if (contact?.address) text('p', `USB ${destination(contact.usbAddress || contact.address, contact.usbPort || DEFAULT_USB_PORT)} · control ${destination(contact.address, contact.controlPort || DEFAULT_CONTROL_PORT)}`, more);
    text('output', peer, more);
    button('Remove pairing', () => act('Pairing removed', async () => {
      await host('revoke', { fingerprint: peer }); delete contacts[peer]; save(PROFILE_KEY + profileIdentity, contacts);
    }, `Remove pairing with ${peerName(peer)}? Its active connections will start releasing.`), more);
  }
}
function openShare(target) {
  return act('Sharing options opened', async () => {
    shareTarget = target; shareInspection = null;
    $('share-device').textContent = deviceName(target);
    $('storage-unmounted').checked = false; $('share-message').textContent = '';
    $('storage-confirmation').hidden = true;
    $('share-dialog').showModal();
    const registry = target.registry || physical.find(p => p.id === target.key?.id)?.registry;
    shareInspection = target.key
      ? await control('inspect', target.key.id, target.key.generation)
      : await host('inspect', { registry });
    $('storage-confirmation').hidden = !shareInspection.storage;
    if ([USB_SUPER_SPEED, USB_SUPER_PLUS_SPEED].includes(shareInspection.speed)) {
      $('share-message').textContent = 'The Windows USB driver cannot import USB 3 devices at SuperSpeed. Reconnect this device through a USB 2 port or hub, then refresh local devices.';
    }
  });
}
function drawDevices() {
  const signature = JSON.stringify([inventory, physical, sessions, status?.role, contacts, status?.peers]);
  if (signature === renderedDevices) return;
  renderedDevices = signature;
  for (const id of ['physical', 'shared-devices', 'remote-devices']) $(id).replaceChildren();
  for (const d of physical.filter(d => status?.role !== 'exporter' || !inventory.some(selected => selected.key.id === d.id))) {
    const row = box($('physical'), 'device-card'); const heading = box(row, 'row'); const copy = box(heading, '');
    text('h4', deviceName(d), copy);
    const actions = deviceActions(heading, 'Share', () => openShare(d), () => inspect(async () => (await host('inspect', { registry: d.registry })).text), 'exporter');
    if (sessions.some(s => s.state !== 'Closed' && s.devices.some(key => key.id === d.id))) {
      actions.firstElementChild.dataset.blocked = '';
      actions.firstElementChild.dataset.tooltip = 'Wait for the previous connection to finish releasing.';
      text('p', 'Releasing', copy);
    }
  }
  if (status?.role === 'exporter') for (const d of inventory) {
    const row = box($(d.shared ? 'shared-devices' : 'physical'), 'device-card'); const args = [d.key.id, d.key.generation];
    const heading = box(row, 'row'); const copy = box(heading, '');
    text('h4', deviceName(d), copy);
    if (d.busy) text('span', d.shared ? 'In use' : 'Releasing', copy, 'badge');
    const actions = deviceActions(heading, d.shared ? 'Stop sharing' : 'Share', d.shared
      ? () => act('Device sharing stopped', async () => {
        await control('unshare', ...args); notice(`${deviceName(d)} is no longer shared.`);
      }, 'Stop sharing this device with every computer? Active connections to it will disconnect.')
      : () => openShare(d), () => inspect(async () => (await control('inspect', ...args)).text), 'exporter');
    if (!d.shared && d.busy) {
      actions.firstElementChild.dataset.blocked = '';
      actions.firstElementChild.dataset.tooltip = 'Wait for the previous connection to finish releasing.';
    }

  }
  if (!$('shared-devices').children.length) text('p', 'No shared devices.', $('shared-devices'), 'empty');
  if (!$('physical').children.length) text('p', status?.exporter ? 'No available devices.' : 'Local sharing is unsupported.', $('physical'), 'empty');
  const remote = status?.role === 'importer' ? inventory : [];
  if (!remote.length) text('p', status?.role === 'importer' ? 'No shared devices. Share a device on the other computer with this computer, then refresh this list.' : 'No computer connected.', $('remote-devices'), 'empty');
  for (const d of remote) {
    const row = box($('remote-devices'), 'device-card'); const heading = box(row, 'row'); const copy = box(heading, '');
    text('h4', deviceName(d), copy); text('p', d.busy ? 'Currently in use' : 'Ready to connect', copy);
    deviceActions(row, 'Use device', () => act('Device connection requested', async () => {
      if ([USB_SUPER_SPEED, USB_SUPER_PLUS_SPEED].includes(d.speed)) throw new Error('The Windows USB driver cannot import USB 3 devices at SuperSpeed. Reconnect the device through a USB 2 port or hub on the sharing computer, then refresh both device lists.');
      if (status.driver === 'missing') throw new Error('The USB/IP driver is missing on this computer. Run the KnightBus installer again before using remote devices.');
      await control('attach', d.key.id, d.key.generation); selectTab('tab-computers');
    }), () => inspect(async () => (await control('inspect', d.key.id, d.key.generation)).text), 'importer');
  }
}
const SESSION_LABELS = { Active: 'In use', Acquiring: 'Connecting', Draining: 'Releasing', Quarantined: 'Needs attention', Closed: 'Disconnected' };
function drawSessions() {
  const signature = JSON.stringify([sessions, contacts, inventory, physical]);
  if (signature === renderedSessions) return;
  renderedSessions = signature; $('session-list').replaceChildren();
  $('connection-count').textContent = sessions.filter(s => s.state !== 'Closed').length;
  if (!sessions.length) text('p', 'No active connections.', $('session-list'), 'empty');
  for (const peer of new Set(sessions.map(s => s.peer))) {
    const row = box($('session-list'), 'session-card');
    text('h3', peerName(peer), row);
    const list = text('ul', '', row, 'session-devices');
    for (const s of sessions.filter(s => s.peer === peer)) {
      const item = text('li', '', list);
      const heading = box(item, 'row');
      const names = s.devices.map(key => {
        // Importer discovery generations change on refresh; identity stays stable.
        const device = inventory.find(d => d.key.id === key.id);
        const local = status?.role === 'exporter' && physical.find(d => d.id === key.id);
        return device ? deviceName(device) : local ? deviceName(local) : `Device ${key.id}`;
      });
      text('h4', names.join(', ') || 'Device connection', heading);
      text('span', SESSION_LABELS[s.state] || s.state, heading, 'badge');
      if (s.error) text('p', s.error, item);
      if (s.state === 'Quarantined') text('p', 'The device is still held. Wait for cleanup before using it again.', item);
      const more = details(item, 'Connection details');
      text('pre', `${s.state}\nSession ${s.id.epoch} / ${s.id.serial}\nComputer ${s.peer}\nDevices ${s.devices.map(d => `${d.id} / ${d.generation}`).join(', ')}`, more);
      if (s.state !== 'Closed') button('Disconnect', () => act('Disconnection requested', () => control('detach', s.id.epoch, s.id.serial), 'Disconnect this session and release its USB device?'), item);
      else button('Dismiss', () => act('Closed connection dismissed', () => control('forget', s.id.epoch, s.id.serial)), item);
    }
  }
}
async function loadPhysical() {
  physical = [];
  try { if (status.exporter) physical = (await host('physical')).host.value; }
  finally { renderedDevices = ''; drawDevices(); }
}
async function refresh(includeInventory = false) {
  try {
    const info = await control('info');
    const changed = epoch !== info.epoch;
    if (epoch !== null && changed) { inventory = []; sessions = []; connectedPeer = null; clearPairing('Service restarted. Show a new PIN.'); log('Service restarted. Device selections were reset.'); }
    if (changed) discoveryApplied = '';
    epoch = info.epoch;
    status = (await host('status')).host.value;
    if (profileIdentity !== status.fingerprint) {
      profileIdentity = status.fingerprint; const saved = readSaved(PROFILE_KEY + profileIdentity);
      contacts = saved && typeof saved === 'object' && !Array.isArray(saved) ? saved : {};
    }
    connected = true; serviceFailure = ''; startupFailure = ''; $('service-recovery').hidden = true;
    $('capabilities').textContent = status.guidance; $('selection').textContent = status.selection;
    $('fingerprint').textContent = status.fingerprint;
    $('exporter-support').hidden = status.exporter;
    $('exporter-support').textContent = 'Local sharing is unsupported in this build.';
    $('importer-support').hidden = status.importer;
    $('importer-support').textContent = 'Remote USB is unsupported in this build.';
    $('driver-setup').hidden = !status.importer || !['missing', 'unavailable'].includes(status.driver);
    $('driver-message').textContent = status.driver === 'missing' ? 'The USB/IP driver is missing. Run the KnightBus installer again to install it, then restart Windows if requested.' : 'KnightBus could not find a working USB/IP controller. Run the KnightBus installer again and check the controller in Device Manager.';
    if (includeInventory || status.role !== 'importer') {
      try {
        inventory = (await control('list')).devices;
        $('inventory-status').textContent = status.role === 'importer' ? `Showing devices from ${connectedPeer ? peerName(connectedPeer) : 'the current remote computer'}.` : '';
      } catch (e) { inventory = []; $('inventory-status').textContent = `Cannot load shared devices: ${e}`; }
    }
    const ids = (await control('sessions')).sessions; const updated = [];
    for (const id of ids) updated.push((await control('status', id.epoch, id.serial)).session);
    sessions = updated;
    if (changed) {
      try { await loadPhysical(); } catch (e) { error(`Cannot list local devices: ${e}`); }
    }
    drawPeers(); drawNearby(); drawDevices(); drawSessions();
    await syncDiscovery();
    if (pairingActive) await pollPairing();
  } catch (e) {
    connected = false; inventory = []; sessions = []; physical = []; connectedPeer = null;
    for (const id of ['physical', 'shared-devices', 'remote-devices', 'session-list']) $(id).replaceChildren();
    text('p', 'Service disconnected.', $('physical'), 'empty');
    text('p', 'Service disconnected.', $('remote-devices'), 'empty');
    renderedDevices = ''; renderedSessions = ''; renderedNearby = null; nearbyComputers = []; discoveryChecked = false; $('hints').replaceChildren(); $('connection-count').textContent = '0';
    clearPairing('Service disconnected. Reconnect and start pairing again.');
    showServiceFailure(e);
  } finally { controls(); }
}
async function syncDiscovery() {
  if (!connected || !status.discovery) return;
  const signature = JSON.stringify([$('endpoint').value, epoch, discoveryPreference, $('computer-name').value, $('local-address').value, $('usb-port').value, $('control-port').value]);
  if (signature === discoveryApplied) return;
  // Apply only after a preference or service change, never on every polling tick.
  discoveryApplied = signature;
  try {
    await host('advertise', { enabled: discoveryPreference,
      name: effective('computer-name'),
      usb: discoveryPreference ? destination(effective('local-address'), $('usb-port').value) : '',
      control: discoveryPreference ? destination(effective('local-address'), $('control-port').value) : '' });
    discoveryFailure = '';
  } catch (e) {
    discoveryFailure = `LAN discovery could not be enabled: ${e}. Check this computer’s IP in Settings and install the matching app and service.`;
    error(discoveryFailure);
  }
}
function openPairing(target = null) {
  pinTarget = target?.fingerprint ? target : null;
  $('pin-progress').textContent = status.pin_pairing ? 'Show a PIN on one computer, then enter it on the other.' : 'Update the app and service to use PIN pairing.';
  $('pin-pairing').hidden = !status.pin_pairing;
  $('pairing-advanced').open = !status.pin_pairing;
  $('pin-input').value = '';
  drawPinComputers();
  if (pinTarget) $('pin-progress').textContent = `Show a PIN on ${pinTarget.name}, then enter it here.`;
  $('pair-name').value = $('computer-name').value; $('pair-address').value = $('local-address').value;
  $('pair-name').placeholder = $('computer-name').placeholder; $('pair-address').placeholder = $('local-address').placeholder;
  $('pair-message').textContent = ''; $('pairing').showModal();
  try { ownCode(); } catch (e) { $('pair-message').textContent = String(e); }
  if (status.discovery) void act('Nearby computers checked', findComputers);
}
function ownCode() {
  const code = encodeCard({ name: effective('pair-name'), address: effective('pair-address'), fingerprint: status.fingerprint, certificate: status.certificate, usbPort: $('usb-port').value, controlPort: $('control-port').value });
  $('own-code').value = code; $('pair-fingerprint').textContent = status.fingerprint; return code;
}
function savePreferences() {
  save(PREFERENCES_KEY, { name: $('computer-name').value.trim().slice(0, MAX_NAME_LENGTH), address: $('local-address').value.trim(), nameOverride: !!$('computer-name').value.trim(), addressOverride: !!$('local-address').value.trim(), discoverable: discoveryPreference, endpoint: connected ? $('endpoint').value.trim() : readSaved(PREFERENCES_KEY).endpoint });
}
for (const b of document.querySelectorAll('[data-open-pair]')) b.addEventListener('click', () => openPairing());
for (const b of document.querySelectorAll('[data-close]')) b.addEventListener('click', () => $(b.dataset.close).close());
$('computer-name').addEventListener('change', savePreferences);
$('local-address').addEventListener('change', () => { $('listen-address').placeholder = effective('local-address'); savePreferences(); });
$('announce').addEventListener('change', () => { discoveryPreference = $('announce').checked; savePreferences(); act('LAN discovery updated', syncDiscovery); });
for (const id of ['computer-name', 'local-address', 'usb-port', 'control-port']) $(id).addEventListener('change', () => act('LAN discovery updated', syncDiscovery));
for (const id of ['pair-name', 'pair-address']) $(id).addEventListener('input', () => {
  try { ownCode(); $('pair-message').textContent = ''; } catch (e) { $('own-code').value = ''; $('pair-message').textContent = String(e); }
});
$('copy-code').addEventListener('click', async () => {
  try {
    const code = ownCode(); $('computer-name').value = $('pair-name').value; $('local-address').value = $('pair-address').value; savePreferences();
    try { await navigator.clipboard.writeText(code); $('pair-message').textContent = 'Copied. Send this code to your other computer.'; }
    catch { $('own-code').closest('details').open = true; $('own-code').select(); $('pair-message').textContent = 'Select and copy the code above, then send it to your other computer.'; }
  } catch (e) { $('pair-message').textContent = String(e); }
});
$('pair').addEventListener('submit', event => {
  event.preventDefault();
  let contact;
  try { contact = decodeCard(new FormData(event.target).get('code')); }
  catch (e) { $('pair-message').textContent = String(e); return; }
  act('Computer paired; no devices shared', async () => {
    await host('pair', { certificate: contact.certificate, fingerprint: contact.fingerprint });
    // Friendly labels are local convenience only; daemon pairing is authoritative.
    if (Object.keys(contacts).length >= MAX_CONTACTS && !contacts[contact.fingerprint]) delete contacts[Object.keys(contacts)[0]];
    const { name, address, usbPort, controlPort } = contact;
    contacts[contact.fingerprint] = { name, address, usbPort, controlPort }; save(PROFILE_KEY + profileIdentity, contacts);
    $('pairing').close();
    notice(`Paired here with ${contact.name}. Add this computer’s code on the other computer to finish, then share a device.`);
    event.target.reset();
  }, `Pair with ${contact.name}?\n\nCompare this entire fingerprint with the one shown on the other computer, using a channel you trust:\n\n${contact.fingerprint}\n\nOnly confirm if they match. The name and address are not proof of identity.`);
});
async function clearForSwitch() {
  if (status.role === 'idle') return true;
  if (!await confirmAction('Change the selected device or computer? All connections must be closed first. Existing sharing permissions will be removed.')) return false;
  await host('clear'); inventory = []; connectedPeer = null; return true;
}
$('share').addEventListener('submit', event => {
  event.preventDefault(); const peer = $('share-peer').value; const target = shareTarget;
  const registry = target?.registry || physical.find(p => p.id === target?.key?.id)?.registry;
  if (!peer || !target) return;
  if (!shareInspection) return error('Device inspection did not finish. Close this dialog and refresh local devices before trying again.');
  if (shareInspection.storage && !$('storage-unmounted').checked) return error('Eject all volumes on this storage device in Finder, then confirm that they are unmounted before sharing.');
  act('Device access allowed', async () => {
    if (status.capture_ready === false) {
      notice('macOS authorization is required to take control of USB devices.');
      const endpoint = await invoke('enable_capture', { endpoint: $('endpoint').value.trim() });
      $('endpoint').value = endpoint; epoch = null;
      await refresh(true); savePreferences();
      if (!connected || !status.capture_ready) throw new Error('USB sharing setup did not complete.');
    }
    let device = target.key && inventory.some(d => d.key.id === target.key.id && d.key.generation === target.key.generation) ? target : status.role === 'exporter' ? inventory.find(d => d.key.id === (target.id || target.key?.id)) : null;
    if (!device) {
      const fields = { registry, usb: destination($('listen-address').value.trim() || effective('local-address'), $('usb-port').value), control: destination($('listen-address').value.trim() || effective('local-address'), $('control-port').value), capture: true, storage_unmounted: $('storage-unmounted').checked, announce: false };
      if (status.role !== 'exporter' && !await clearForSwitch()) return;
      await host('export', fields);
      inventory = (await control('list')).devices;
      device = inventory.find(d => d.key.id === (target.id || target.key?.id));
      if (!device) throw new Error('This device changed. Refresh local devices and select it again.');
    }
    await control('share', device.key.id, device.key.generation, peer);
    $('share-dialog').close(); notice(`${deviceName(target)} can now be used by ${peerName(peer)}.`);
  });
});
function fillRemoteAddress(reset = false) {
  if (reset) $('remote-address').value = '';
  $('remote-address').placeholder = contacts[$('exporter-peer').value]?.address || 'Automatic';
}
$('exporter-peer').addEventListener('change', () => fillRemoteAddress(true));
async function remoteDestination(peer) {
  let contact = contacts[peer];
  const manual = $('remote-address').value.trim();
  // LAN addresses may change after pairing. TLS still verifies the paired identity.
  if (!manual && status.discovery) {
    try {
      const nearby = (await host('discover')).host.value;
      const computer = nearby.find(c => c.fingerprint === peer);
      if (computer) {
        const controlAddress = new URL(`http://${computer.control}`);
        const usbAddress = new URL(`http://${computer.usb}`);
        contact = { ...contact, address: controlAddress.hostname, usbAddress: usbAddress.hostname,
          usbPort: Number(usbAddress.port), controlPort: Number(controlAddress.port) };
        contacts[peer] = contact; save(PROFILE_KEY + profileIdentity, contacts); fillRemoteAddress();
      }
    } catch { /* The paired address remains usable when LAN discovery is unavailable. */ }
  }
  const address = manual || contact?.address;
  if (!address) {
    $('remote-advanced').open = true;
    throw new Error('Cannot find this computer. Open KnightBus on it and enable LAN discovery, or enter its IP under Advanced connection settings.');
  }
  return { fingerprint: peer,
    usb: destination(manual || contact?.usbAddress || address, contact?.usbPort ?? DEFAULT_USB_PORT),
    control: destination(address, contact?.controlPort ?? DEFAULT_CONTROL_PORT) };
}
$('import').addEventListener('submit', event => {
  event.preventDefault(); const peer = $('exporter-peer').value;
  act('Remote computer selected', async () => {
    const fields = await remoteDestination(peer);
    if (status.role !== 'importer' || connectedPeer !== peer || connectedDestination !== JSON.stringify(fields)) {
      if (!await clearForSwitch()) return;
      await host('import', fields); connectedPeer = peer; connectedDestination = JSON.stringify(fields);
    }
    await refresh(true);
  });
});
$('connect').addEventListener('submit', event => {
  event.preventDefault(); act('Service connection refreshed', async () => { startupFailure = ''; epoch = null; connectedPeer = null; await refresh(true); if (connected) savePreferences(); });
});
$('inventory-refresh').addEventListener('click', () => act('Shared devices refreshed', () => refresh(true)));
$('connections-refresh').addEventListener('click', () => act('Connections refreshed', () => refresh()));
$('paired-refresh').addEventListener('click', () => act('Paired computers refreshed', async () => { if (status.discovery) await findComputers(); await refresh(); }));
$('physical-refresh').addEventListener('click', () => act('Local devices refreshed', loadPhysical));
function drawPinComputers() {
  const previous = $('pin-peer').value;
  const selected = pinTarget?.fingerprint || previous;
  const computers = nearbyComputers.filter(c => c.fingerprint !== status?.fingerprint && !status?.peers.includes(c.fingerprint));
  // Keep the chosen identity through a temporary discovery gap or a new PIN port.
  if (pinTarget && !computers.some(c => c.fingerprint === selected) && !status?.peers.includes(selected)) computers.push(pinTarget);
  $('pin-peer').replaceChildren(new Option('Choose a computer', ''));
  for (const computer of computers) $('pin-peer').append(new Option(computer.name, computer.fingerprint));
  $('pin-peer').value = computers.some(c => c.fingerprint === selected) ? selected : (computers[0]?.fingerprint || '');
  pinTarget = computers.find(c => c.fingerprint === $('pin-peer').value) || null;
}
$('pin-peer').addEventListener('change', () => {
  pinTarget = nearbyComputers.find(c => c.fingerprint === $('pin-peer').value) || null;
  $('pin-endpoint').value = '';
  $('pin-progress').textContent = pinTarget ? `Show a PIN on ${pinTarget.name}, then enter it here.` : 'Choose a computer, then enter its PIN.';
});
function drawNearby() {
  const signature = JSON.stringify([nearbyComputers, status.peers, discoveryChecked, discoveryFailure]);
  if (signature === renderedNearby) return;
  renderedNearby = signature;
  $('hints').replaceChildren();
  const unpaired = nearbyComputers.filter(c => c.fingerprint !== status.fingerprint && !status.peers.includes(c.fingerprint));
  if (unpaired.length) {
    text('h4', 'Nearby computers', $('hints'), 'section-label');
    for (const computer of unpaired) {
      const row = box($('hints'), 'peer-card');
      const heading = box(row, 'row');
      text('h3', computer.name, heading);
      button('Pair', () => openPairing(computer), heading);
    }
  } else if (discoveryChecked && !nearbyComputers.some(c => c.fingerprint !== status.fingerprint)) {
    text('p', discoveryFailure || 'No nearby computers found. Enable “Make computer discoverable on LAN” on both computers, use the same LAN and allow KnightBus through the private-network firewall.', $('hints'), 'empty');
  }
  drawPinComputers();
}
async function findComputers() {
  discoveryApplied = ''; await syncDiscovery();
  nearbyComputers = (await host('discover')).host.value;
  discoveryChecked = true;
  drawPeers(); drawNearby(); drawPinComputers();
}
$('browse').addEventListener('click', () => act('Nearby computers checked', findComputers));
$('pin-find').addEventListener('click', () => act('Nearby computers checked', findComputers));
function localPairingFields() {
  return { name: effective('computer-name'), usb: destination(effective('local-address'), $('usb-port').value), control: destination(effective('local-address'), $('control-port').value) };
}
function clearPairing(message) {
  pairingActive = false; $('pin-value').textContent = ''; $('pin-value').hidden = true;
  $('pin-listen').textContent = ''; $('pin-listen-details').hidden = true; $('pin-input').value = '';
  $('pin-progress').textContent = message; controls();
}
function rememberComputer(computer) {
  const controlAddress = new URL(`http://${computer.control}`);
  const usbAddress = new URL(`http://${computer.usb}`);
  if (Object.keys(contacts).length >= MAX_CONTACTS && !contacts[computer.fingerprint]) delete contacts[Object.keys(contacts)[0]];
  contacts[computer.fingerprint] = { name: computer.name.slice(0, MAX_NAME_LENGTH), address: controlAddress.hostname, usbAddress: usbAddress.hostname, usbPort: Number(usbAddress.port), controlPort: Number(controlAddress.port) };
  save(PROFILE_KEY + profileIdentity, contacts);
}
async function pollPairing() {
  const progress = (await host('pairing_status')).host.value;
  if (progress.state === 'waiting') $('pin-progress').textContent = `Waiting for the other computer. PIN expires in ${progress.expires_in}s. Attempts left: ${progress.attempts_left}.`;
  else if (progress.state === 'connecting') $('pin-progress').textContent = 'Pairing…';
  else if (progress.state === 'paired') {
    rememberComputer(progress.computer);
    clearPairing('Pairing complete.');
    $('pairing').close();
    notice(`Paired with ${progress.computer.name}. You can now share devices with this computer.`);
    status = (await host('status')).host.value; drawPeers(); drawNearby();
    log('Computers paired with PIN; no devices shared');
  } else if (progress.state === 'failed') {
    const message = progress.error === 'Deadline' ? 'Pairing timed out. Show a new PIN and try again.'
      : progress.error === 'Denied' ? 'PIN pairing was rejected. Check the PIN or show a new one.'
      : 'Pairing did not finish. Check the address, PIN and private-network firewall, then retry. Check Computers if the connection was interrupted.';
    clearPairing(message);
  } else clearPairing('Pairing stopped.');
}
$('pin-start').addEventListener('click', () => act('PIN pairing opened', async () => {
  const offer = (await host('pairing_start', localPairingFields())).host.value;
  if (!$('pairing').open) { await host('pairing_cancel'); clearPairing('Pairing cancelled.'); return; }
  pairingActive = true;
  $('pin-value').textContent = offer.pin; $('pin-value').hidden = false;
  $('pin-listen').textContent = offer.address; $('pin-listen-details').hidden = false;
  $('pin-progress').textContent = 'Enter this PIN on the other computer.';
}));
$('pin-join').addEventListener('submit', event => {
  event.preventDefault();
  const pin = $('pin-input').value.trim();
  const manual = $('pin-endpoint').value.trim();
  const selected = $('pin-peer').value;
  if ((!manual && !selected) || !new RegExp(`^[0-9]{${PIN_DIGITS}}$`).test(pin)) { $('pin-progress').textContent = 'Choose a computer and enter its six-digit PIN.'; return; }
  act('PIN pairing started', async () => {
    let address = manual;
    if (!address) {
      // Resolve the current PIN listener only when pairing, not when selecting.
      await findComputers();
      const computer = nearbyComputers.find(c => c.fingerprint === selected);
      if (!computer?.pairing_port) {
        $('pin-progress').textContent = computer ? `Choose Show PIN on ${computer.name}, then enter that PIN here and press Pair.` : 'This computer is no longer nearby. Check its connection and try again.';
        return;
      }
      const url = new URL(`http://${computer.control}`);
      address = `${url.hostname}:${computer.pairing_port}`;
    }
    await host('pairing_join', { ...localPairingFields(), address, pin });
    if (!$('pairing').open) { await host('pairing_cancel'); clearPairing('Pairing cancelled.'); return; }
    $('pin-input').value = ''; pairingActive = true; $('pin-progress').textContent = 'Pairing…';
  });
});
async function cancelPairing() {
  if (!pairingActive) return;
  if (busy) { pairingCancelQueued = true; return; }
  await act('PIN pairing cancelled', async () => { await host('pairing_cancel'); clearPairing('Pairing cancelled.'); });
}
$('pin-cancel').addEventListener('click', cancelPairing);
$('pairing').addEventListener('close', () => { void cancelPairing(); $('pin-input').value = ''; $('pin-value').textContent = ''; });
$('diagnostics').addEventListener('click', () => act('Diagnostics collected', async () => {
  const report = { complete: false, collected: new Date().toISOString(), statuses: [] };
  try {
    report.host = await control('info'); const ids = (await control('sessions')).sessions;
    for (const id of ids) report.statuses.push((await control('status', id.epoch, id.serial)).session);
    report.complete = true;
  } catch (e) { report.error = String(e); throw e; }
  finally { $('report').value = JSON.stringify(report, null, JSON_INDENT); }
}));
function showServiceFailure(value) {
  serviceFailure = startupFailure || String(value);
  $('service-recovery').hidden = false;
  $('service-error').textContent = `Endpoint: ${$('endpoint').value}\n${serviceFailure}`;
  const version = serviceFailure.startsWith('Version');
  const denied = serviceFailure.startsWith('Denied');
  $('service-title').textContent = version ? 'Service version mismatch' : denied ? 'Service access denied' : 'Service disconnected';
  $('service-message').textContent = startupFailure ? startupFailure.split('\n')[0] : version
    ? 'Install matching app and service versions, then retry.'
    : denied ? 'This account cannot access the selected service. Check the endpoint or repair service permissions with its installer.'
    : 'The app cannot reach its background process. Start a local service or retry an existing one.';
  $('start-service').hidden = version || denied;
}
$('retry-service').addEventListener('click', () => act('Service connection retried', () => { startupFailure = ''; return refresh(true); }));
$('service-settings').addEventListener('click', () => {
  selectTab('tab-settings'); $('advanced').open = true; $('endpoint').focus();
});
$('service-help').addEventListener('click', () => $('service-guide').showModal());
$('start-service').addEventListener('click', async () => {
  if (busy || serviceStarting) return;
  serviceStarting = true; startupFailure = ''; busy = true; controls(); $('start-service').textContent = 'Starting…';
  $('error').hidden = true;
  try {
    await backgroundRefresh;
    if (localEndpoint) $('endpoint').value = localEndpoint;
    const result = await invoke('start_service');
    $('endpoint').value = result.endpoint; epoch = null; connectedPeer = null;
    await refresh(true);
    if (connected) { savePreferences(); log(result.started ? 'Local service started' : 'Existing local service connected'); }
  } catch (e) {
    startupFailure = String(e); showServiceFailure(e);
    log(`Service startup failed: ${e}`);
  } finally { serviceStarting = false; busy = false; $('start-service').textContent = 'Start local service'; controls(); }
});
async function quitKnightBus() {
  if (busy) return;
  busy = true; controls();
  try {
    await backgroundRefresh;
    if (!await confirmAction('Quit KnightBus? All USB connections will disconnect and the background service will stop.')) return;
    notice('Disconnecting devices and stopping KnightBus…');
    await invoke('quit_service', { endpoint: $('endpoint').value.trim() });
  } catch (e) { $('notice').hidden = true; error(e); }
  finally { busy = false; controls(); }
}
$('quit-app').addEventListener('click', quitKnightBus);
if (window.__TAURI__.event) await window.__TAURI__.event.listen('quit-requested', quitKnightBus);
async function poll() {
  if (!busy && (!document.querySelector('dialog[open]') || (pairingActive && $('pairing').open && !$('consent').open))) {
    backgroundRefresh = refresh();
    try { await backgroundRefresh; } finally { backgroundRefresh = null; }
  }
  setTimeout(poll, REFRESH_MS);
}
const preferences = readSaved(PREFERENCES_KEY);
discoveryPreference = preferences.discoverable !== false;
const nameOverride = preferences.nameOverride ?? (typeof preferences.name === 'string' && !!preferences.name && preferences.name !== 'My computer');
const addressOverride = preferences.addressOverride ?? (typeof preferences.address === 'string' && !!preferences.address);
$('computer-name').value = nameOverride ? (preferences.name || '').slice(0, MAX_NAME_LENGTH) : '';
$('local-address').value = addressOverride ? preferences.address || '' : '';
$('usb-port').value = DEFAULT_USB_PORT; $('control-port').value = DEFAULT_CONTROL_PORT;
controls();
try { const defaults = await invoke('defaults');
  $('listen-address').placeholder = '';
  $('computer-name').placeholder = defaults.name || '';
  $('local-address').placeholder = defaults.address || '';
  $('listen-address').placeholder = effective('local-address') || '0.0.0.0';
  localEndpoint = defaults.localEndpoint || ''; $('endpoint').value = typeof preferences.endpoint === 'string' && preferences.endpoint ? preferences.endpoint : defaults.endpoint; await refresh(true); } catch (e) { error(e); }
setTimeout(poll, REFRESH_MS);
