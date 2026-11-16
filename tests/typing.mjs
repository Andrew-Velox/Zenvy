// Simulates an Android keyboard (composition, autocorrect, suggestion taps, emoji) against the page's real
// text-sync code and checks the PC-side text always equals what is in the field, as it is typed.
import fs from 'node:fs';
const html = fs.readFileSync(new URL('../crates/zenvy-net/src/page.html', import.meta.url), 'utf8');
const src = html.slice(html.indexOf('/*attachText:start*/'), html.indexOf('/*attachText:end*/'));
let pass = 0, fail = 0; const ok = (c, m) => { c ? pass++ : fail++; console.log(`${c ? 'PASS' : 'FAIL'}  ${m}`); };

function rig({ live = true, mods = [] } = {}) {
  const pc = { text: '', log: [] };
  const apply = (o) => { // what the PC would do with each command
    pc.log.push(o);
    if (o.key === 'backspace') pc.text = [...pc.text].slice(0, -1).join('');
    else if (o.key === 'space') pc.text += ' ';
    else if (o.key === 'enter') pc.text += '\n';
    else if (o.key && o.key.length === 1) pc.text += o.mods?.length ? `<${o.mods.join('+')}+${o.key}>` : o.key;
    else if (o.text !== undefined) pc.text += o.text;
  };
  const m = new Set(mods);
  const key = (k, extra) => apply({ key: k, mods: extra || [...m] });
  const queueText = (t) => apply({ text: t });
  const clearMods = () => m.clear();
  const prefs = { live };
  const L = {}; const kb = { value: '', addEventListener: (n, f) => { (L[n] ||= []).push(f); } };
  new Function('kb', 'key', 'queueText', 'mods', 'clearMods', 'prefs', src + '\nattachText(kb);')(kb, key, queueText, m, clearMods, prefs);
  const fire = (n, e = {}) => (L[n] || []).forEach(f => f({ preventDefault() {}, ...e }));
  // an IME edit: change the field, then fire input (isComposing as given)
  const edit = (value, composing) => { kb.value = value; fire('input', { isComposing: composing, inputType: 'insertCompositionText' }); };
  return { pc, kb, fire, edit };
}

// 1. live typing while composing: PC text matches the field after every single keystroke
{
  const r = rig(); r.fire('compositionstart');
  let allMatch = true;
  for (const v of ['h', 'he', 'hel', 'hell', 'hello']) { r.edit(v, true); allMatch &&= r.pc.text === r.kb.value; }
  ok(allMatch, 'live: each letter reaches the PC while the word is still being composed');
  r.fire('compositionend'); r.edit('hello ', false);
  ok(r.pc.text === 'hello ', 'space after the word adds only the space (no duplicate text)');
}
// 2. autocorrect replaces the composing word
{
  const r = rig(); r.fire('compositionstart');
  for (const v of ['t', 'te', 'teh']) r.edit(v, true);
  r.edit('the', true); r.fire('compositionend');
  ok(r.pc.text === 'the', `autocorrect "teh" -> "the" ends as the corrected word (PC: ${JSON.stringify(r.pc.text)})`);
}
// 3. tapping a suggestion replaces the partial word and adds a space
{
  const r = rig(); r.fire('compositionstart');
  for (const v of ['h', 'he', 'hel']) r.edit(v, true);
  r.edit('hello ', true); r.fire('compositionend');
  ok(r.pc.text === 'hello ', 'tapping a suggestion completes the word');
}
// 4. deleting characters
{
  const r = rig(); r.fire('compositionstart');
  for (const v of ['c', 'ca', 'cat']) r.edit(v, true);
  r.edit('ca', true); r.edit('c', true);
  ok(r.pc.text === 'c', 'backspacing inside the field deletes on the PC');
}
// 5. emoji and non-Latin text (code points, not UTF-16 halves)
{
  const r = rig(); r.edit('a', false); r.edit('a\u{1F600}', false); r.edit('a\u{1F600}日', false); r.edit('a\u{1F600}', false);
  ok(r.pc.text === 'a\u{1F600}', 'emoji is typed and deleted as one character');
}
// 6. live typing off: nothing is sent until the word is finished
{
  const r = rig({ live: false }); r.fire('compositionstart');
  for (const v of ['h', 'he', 'hel']) r.edit(v, true);
  ok(r.pc.text === '', 'live off: nothing sent while composing');
  r.edit('hello', true); r.fire('compositionend');
  ok(r.pc.text === 'hello', 'live off: the word is sent when composition ends');
}
// 7. a sticky modifier turns the next letter into a shortcut instead of text
{
  const r = rig({ mods: ['ctrl'] }); r.edit('c', false);
  ok(r.pc.text === '<ctrl+c>', 'Ctrl armed + "c" sends the Ctrl+C shortcut, not the letter');
}
// 8. long sessions: the field clears at a word boundary and no text is lost
{
  const r = rig(); let typed = ''; let field = '';
  for (let w = 0; w < 40; w++) { const word = 'word' + w + ' '; for (const ch of word) { field += ch; typed += ch; r.edit(field, false); if (r.kb.value === '' ) field = ''; } }
  ok(r.pc.text === typed, `120+ characters typed: the field reset at a word boundary and the PC still got every character (${typed.length} chars)`);
  ok(r.kb.value.length <= 125, 'the field itself stays short');
}
// 9. Enter is a key, never text
{
  const r = rig(); r.edit('hi', false); r.fire('beforeinput', { inputType: 'insertLineBreak' });
  ok(r.pc.text === 'hi\n', 'Enter sends an Enter key press');
}
console.log(`\n${pass} passed, ${fail} failed`); process.exit(fail ? 1 : 0);
