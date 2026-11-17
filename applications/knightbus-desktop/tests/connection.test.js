import {test} from 'node:test';
import assert from 'node:assert/strict';
import {encodeCard,decodeCard,destination,networkAddress} from '../frontend/connection.js';
const card={name:'User’s Mac',fingerprint:'ab'.repeat(32),certificate:'cafe',address:'192.168.1.10',usbPort:3337,controlPort:3338};
test('public connection codes round-trip Unicode and omit unrecognized fields',()=>{
  assert.deepEqual(decodeCard(encodeCard({...card,secret:'must not be copied'})),card);
});
test('malformed or oversized contact data is rejected',()=>{
  for(const change of [{fingerprint:'abc'},{certificate:'abc'},{usbPort:65536},{controlPort:0},{name:'x'.repeat(65)},{address:'example.com'},{name:'\nPC'}]) assert.throws(()=>encodeCard({...card,...change}));
  for(const code of ['garbage','knightbus:1:!!!','knightbus:1:'+ 'x'.repeat(50000)]) assert.throws(()=>decodeCard(code));
});
test('IPv4 and IPv6 destinations have one validated port',()=>{
  assert.equal(destination('192.168.1.10',3337),'192.168.1.10:3337');
  assert.equal(destination('2001:db8::1',3338),'[2001:db8::1]:3338');
  assert.equal(destination('[::1]',3337),'[::1]:3337');
  for(const address of ['256.0.0.1','127.1','192.168.1.2:3337','[::1]:3337','example.com','::g']) assert.throws(()=>networkAddress(address));
});
