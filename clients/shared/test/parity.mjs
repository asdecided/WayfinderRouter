// Decision-preview contract gate (WF-ADR-0042): compare the shared JS preview against the
// frozen migration corpus byte-for-byte. Run from the repo root:
//
//     node clients/shared/test/parity.mjs
//
// Exits non-zero on ANY divergence (score, routing, or a single feature). A client embedding this
// scorer may trust it as a degraded-mode decision source ONLY while this gate is green.
import {readFileSync} from 'node:fs';
import {scoreComplexity, FEATURE_ORDER} from '../src/scorer.js';

const golden = JSON.parse(readFileSync(new URL('./golden.json', import.meta.url)));
let scoreFail = 0, recFail = 0, featFail = 0;
const rows = [];
for (const g of golden) {
  const js = scoreComplexity(g.text);
  const sOk = js.score === g.score;
  const rOk = js.recommendation === g.recommendation;
  const fDiff = FEATURE_ORDER.filter((f) => js.features[f] !== g.features[f]);
  if (!sOk) scoreFail++;
  if (!rOk) recFail++;
  if (fDiff.length) featFail++;
  const mark = sOk && rOk && !fDiff.length ? '✅' : '❌';
  rows.push(`${mark} ${g.name.padEnd(18)} fixture=${g.score.toFixed(2)} js=${js.score.toFixed(2)} `
    + `${sOk ? 'score✓' : 'SCORE✗'} ${rOk ? 'route✓' : 'ROUTE✗'} `
    + (fDiff.length ? `FEAT✗(${fDiff.map((f) => `${f}:fixture${g.features[f]}/js${js.features[f]}`).join(',')})` : 'feat✓'));
}
console.log(rows.join('\n'));
const n = golden.length;
console.log(`\nscore parity: ${n - scoreFail}/${n} · routing parity: ${n - recFail}/${n} · feature parity: ${n - featFail}/${n}`);
process.exit(scoreFail + recFail + featFail === 0 ? 0 : 1);
