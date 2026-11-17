// A public contact card, not a secret or an authorization token. The daemon still
// verifies the certificate against the independently confirmed fingerprint.
// KnightBus defaults; both ports remain configurable for existing deployments.
export const DEFAULT_USB_PORT = 3337;
export const DEFAULT_CONTROL_PORT = 3338;
export const MAX_NAME_LENGTH = 64;
const MAX_PORT = 65535;
const MAX_IPV4_OCTET = 255;
const IPV4_OCTETS = 4;
const FINGERPRINT_HEX_LENGTH = 64;
const MAX_CERTIFICATE_HEX_LENGTH = 32 * 1024;
const MAX_CODE_LENGTH = 48 * 1024;
const CODE_PREFIX = 'knightbus:1:';
const HEX_DIGITS_PER_BYTE = 2;

export function networkAddress(value) {
  const address = value.trim().replace(/^\[|\]$/g, '');
  if (address.includes(':')) {
    try { return new URL(`http://[${address}]/`).hostname; } catch { /* Report below. */ }
  } else {
    const parts = address.split('.');
    if (parts.length === IPV4_OCTETS && parts.every(p => /^\d+$/.test(p) && Number(p) <= MAX_IPV4_OCTET)) return parts.map(Number).join('.');
  }
  throw new Error('Enter this computer’s IP address, such as 192.168.1.10. Ports belong in Advanced settings.');
}
export function port(value) {
  const number = Number(value);
  if (!Number.isInteger(number) || number < 1 || number > MAX_PORT) throw new Error('Choose a port between 1 and 65535.');
  return number;
}
export function destination(address, value) { return `${networkAddress(address)}:${port(value)}`; }
function validate(card) {
  if (!card || typeof card !== 'object' || Array.isArray(card)
    || typeof card.name !== 'string' || !card.name.trim() || card.name.length > MAX_NAME_LENGTH || /[\x00-\x1f\x7f]/.test(card.name)
    || typeof card.fingerprint !== 'string' || !new RegExp(`^[a-fA-F0-9]{${FINGERPRINT_HEX_LENGTH}}$`).test(card.fingerprint)
    || typeof card.certificate !== 'string' || !/^[a-fA-F0-9]+$/.test(card.certificate)
    || card.certificate.length > MAX_CERTIFICATE_HEX_LENGTH || card.certificate.length % HEX_DIGITS_PER_BYTE !== 0
    || typeof card.address !== 'string') throw new Error('This connection code is incomplete or invalid. Copy a new code from the other computer.');
  return { name: card.name.trim(), fingerprint: card.fingerprint.toLowerCase(), certificate: card.certificate.toLowerCase(), address: card.address ? networkAddress(card.address) : '', usbPort: port(card.usbPort), controlPort: port(card.controlPort) };
}
export function encodeCard(card) {
  const bytes = new TextEncoder().encode(JSON.stringify(validate(card)));
  return CODE_PREFIX + btoa(Array.from(bytes, b => String.fromCharCode(b)).join(''));
}
export function decodeCard(value) {
  const code = value.trim();
  if (!code.startsWith(CODE_PREFIX) || code.length > MAX_CODE_LENGTH) throw new Error('Paste a complete KnightBus connection code from the other computer.');
  try { return validate(JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(Uint8Array.from(atob(code.slice(CODE_PREFIX.length)), c => c.charCodeAt(0))))); }
  catch { throw new Error('This connection code is incomplete or invalid. Copy a new code from the other computer.'); }
}
