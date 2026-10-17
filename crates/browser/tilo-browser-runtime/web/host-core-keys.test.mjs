import test from 'node:test';
import assert from 'node:assert/strict';
import {isDesktopChord,keyboardRoute,semanticKeyboardRoute} from './host-core.mjs';

const key=(code,mods={})=>({key:code.replace(/^(Key|Digit)/,''),code,...mods});

test('desktop chords are the shell\'s, with Super or the page\'s stand-in for it',()=>{
  for(const code of ['ArrowLeft','ArrowRight','ArrowUp','ArrowDown','PageUp','PageDown','KeyO','Digit1','Digit9']){
    assert.ok(isDesktopChord(key(code,{metaKey:true}),false),`Super+${code}`);
    assert.ok(isDesktopChord(key(code,{altKey:true,shiftKey:true}),false),`Alt+Shift+${code}`);
    assert.ok(isDesktopChord(key(code,{ctrlKey:true,altKey:true}),true),`Ctrl+Option+${code} on a Mac`);
    assert.equal(keyboardRoute(key(code,{altKey:true,shiftKey:true}),false,false),'forward');
    assert.equal(semanticKeyboardRoute(key(code,{metaKey:true}),false,false),'forward');
  }
  assert.ok(isDesktopChord({key:'Tab',code:'Tab',metaKey:true},false),'Super+Tab opens the overview');
  assert.ok(!isDesktopChord({key:'Tab',code:'Tab',altKey:true,shiftKey:true},false),'the stand-in leaves Tab to focus traversal');
});

test('other modified keys are not desktop chords',()=>{
  assert.ok(!isDesktopChord(key('ArrowLeft',{altKey:true}),false),'Alt+Left is history back');
  assert.ok(!isDesktopChord(key('ArrowLeft',{altKey:true,shiftKey:true}),true),'Option+Shift+Left selects a word on a Mac');
  assert.ok(!isDesktopChord(key('ArrowLeft',{ctrlKey:true,altKey:true}),false),'Ctrl+Alt is AltGr or the system\'s elsewhere');
  assert.ok(!isDesktopChord(key('ArrowLeft',{metaKey:true,ctrlKey:true}),false));
  assert.ok(!isDesktopChord(key('KeyL',{metaKey:true}),false));
  assert.ok(!isDesktopChord({...key('ArrowLeft',{metaKey:true}),isComposing:true},false));
  assert.equal(keyboardRoute(key('ArrowLeft',{altKey:true}),false,false),'browser');
  assert.equal(keyboardRoute(key('ArrowLeft'),false,false),'forward');
});

test('modified text-editing keys are the app\'s, so every text field edits by word and line',()=>{
  for(const code of ['Backspace','Delete','ArrowLeft','ArrowRight','ArrowUp','ArrowDown','Home','End']){
    assert.equal(keyboardRoute(key(code,{ctrlKey:true}),false,false),'forward',`Ctrl+${code}`);
    assert.equal(keyboardRoute(key(code,{ctrlKey:true,shiftKey:true}),false,false),'forward',`Ctrl+Shift+${code}`);
    assert.equal(keyboardRoute(key(code,{altKey:true}),false,true),'forward',`Option+${code} on a Mac`);
    assert.equal(keyboardRoute(key(code,{metaKey:true,shiftKey:true}),false,true),'forward',`Cmd+Shift+${code} on a Mac`);
  }
  assert.equal(keyboardRoute(key('ArrowLeft',{altKey:true}),false,false),'browser','Alt+Left stays history back');
  assert.equal(keyboardRoute(key('KeyL',{ctrlKey:true}),false,false),'browser','Ctrl+L stays the address bar');
});
