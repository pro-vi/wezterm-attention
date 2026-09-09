// Render the lifecycle grid from its Markdown; catalog names are a coverage check.
import { readFileSync, writeFileSync } from 'node:fs';
import { resolve, dirname, basename, join } from 'node:path';
import { createHash } from 'node:crypto';

const sourcePath = resolve(process.argv[2] ?? 'docs/reviews/2026-09-08-attention-hook-map.md');
const catalogPath = sourcePath.replace(/\.md$/, '.catalog.json');
const outputPath = sourcePath.replace(/\.md$/, '.html');
if (sourcePath === outputPath) throw new Error('Expected a Markdown source path');
const source = readFileSync(sourcePath, 'utf8');
const catalogText = readFileSync(catalogPath, 'utf8');
const catalog = JSON.parse(catalogText);
if (catalog.providers?.length !== 3) throw new Error('Expected exactly three provider columns');
const hash = value => createHash('sha256').update(value).digest('hex');
const escape = value => Bun.escapeHTML(String(value));
const textOnly = value => value.replace(/<[^>]+>/g, ' ').replace(/\s+/g, ' ').trim();
let body = Bun.markdown.html(source);
// Editor-style Markdown line links need a real file target in a browser.
body = body.replace(/href="(\/[^"#]+):(\d+)"/g,
  (_, path, line) => `href="${path}#L${line}" title="Source file, reference line ${line}"`);
const title = body.match(/<h1>([\s\S]*?)<\/h1>/)?.[1];
if (!title) throw new Error('The source needs a title');
const headings = [];
body = body.replace(/<h2>([\s\S]*?)<\/h2>/g, (_, label) => {
  const id = textOnly(label).toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');
  if (headings.some(item => item.id === id)) throw new Error('Duplicate heading');
  headings.push({ id, label });
  return `<h2 id="${id}">${label}</h2>`;
});

const found = catalog.providers.map(() => new Set());
const foundNotificationTypes = new Set();
let rowCount = 0;
let tableCount = 0;
body = body.replace(/<table>([\s\S]*?)<\/table>/g, (_, inner) => {
  const headers = [...inner.matchAll(/<th(?:\s[^>]*)?>([\s\S]*?)<\/th>/g)].map(m => textOnly(m[1]));
  if (headers.length !== 5 || headers[0] !== 'Lifecycle observation') throw new Error('Unexpected grid columns');
  tableCount += 1;
  const result = inner.replace(/<tbody>([\s\S]*?)<\/tbody>/, (_, rows) => '<tbody>' +
    rows.replace(/<tr>([\s\S]*?)<\/tr>/g, (_, cellsHTML) => {
      const cells = [...cellsHTML.matchAll(/<td>([\s\S]*?)<\/td>/g)].map(m => m[1]);
      if (cells.length !== 5) throw new Error('Every lifecycle row needs five cells');
      rowCount += 1;
      for (let column = 1; column <= 3; column++) {
        const provider = catalog.providers[column - 1];
        cells[column] = cells[column].replace(/<code>([^<]+)<\/code>/g, (whole, code) => {
          if (column === 1 && catalog.providers[0].notification_types.includes(code)) foundNotificationTypes.add(code);
          if (!provider.hooks.includes(code)) return whole;
          found[column - 1].add(code);
          const href = provider.id === 'pi' ? provider.type_authority : provider.authority + '#' + code.toLowerCase();
          return `<a class="hook" href="${escape(href)}" title="${escape(provider.label)} hook definition">${whole}</a>`;
        });
        cells[column] = cells[column].replace(/<strong>Now:<\/strong>/g, '<strong class="now">Now</strong>');
      }
      const actions = [...textOnly(cells[4]).matchAll(/\b(Next|Later|Keep|Skip|Gap):/g)].map(m => m[1].toLowerCase());
      if (!actions.length) throw new Error('Every row needs an explicit proposal status');
      cells[4] = cells[4].replace(/<strong>(Next|Later|Keep|Skip|Gap):<\/strong>/g,
        (_, kind) => `<strong class="proposal ${kind.toLowerCase()}">${kind}</strong>`);
      return `<tr class="lifecycle-row" data-actions="${[...new Set(actions)].join(' ')}"><th scope="row">${cells[0]}</th>` +
        cells.slice(1).map((cell, i) => `<td data-label="${escape(headers[i + 1])}">${cell}</td>`).join('') + '</tr>';
    }) + '</tbody>');
  return `<table class="lifecycle-grid"><colgroup><col class="event-col"><col><col><col><col class="next-col"></colgroup>${result}</table>`;
});

const coverage = catalog.providers.map((provider, i) => {
  const missing = provider.hooks.filter(hook => !found[i].has(hook));
  if (missing.length) throw new Error(`Unmapped ${provider.label} hooks: ${missing.join(', ')}`);
  return { provider: provider.label, total: provider.hooks.length, mapped: found[i].size, missing };
});
const missingNotifications = catalog.providers[0].notification_types.filter(type => !foundNotificationTypes.has(type));
if (missingNotifications.length) throw new Error('Unmapped Claude notification types: ' + missingNotifications.join(', '));
const parts = body.split(/(?=<h2 id=)/);
const intro = parts.shift();
const sections = parts.map(part => {
  const id = part.match(/<h2 id="([^"]+)"/)?.[1];
  return `<section aria-labelledby="${id}" class="${part.includes('lifecycle-grid') ? 'matrix-section' : 'notes-section'}">${part}</section>`;
}).join('\n');
const nav = headings.map(h => `<a href="#${h.id}">${h.label}</a>`).join('\n');
const report = { source: sourcePath, source_sha256: hash(source), catalog_sha256: hash(catalogText), tables: tableCount,
  lifecycle_rows: rowCount, coverage, claude_notification_types: { mapped: foundNotificationTypes.size, missing: missingNotifications } };

const html = `<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1"><meta name="color-scheme" content="dark">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none'">
<meta name="source-sha256" content="${report.source_sha256}"><title>${title}</title>
<style>
:root{color-scheme:dark;--paper:#0e1720;--surface:#16232e;--ink:#e7eef5;--muted:#b6c6d4;--line:#405766;--link:#9cd1f5;--next:#ffc18a;--keep:#a8dcc2;--gap:#ffb9be;--toolbar-height:66px;--mono:Menlo,Consolas,monospace;--body:-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif}
*{box-sizing:border-box}html{scroll-padding-top:calc(var(--toolbar-height) + 20px)}body{margin:0;background:var(--paper);color:var(--ink);font:16px/1.5 var(--body)}
main{max-width:1900px;margin:auto;padding:28px 30px 60px}header{max-width:122ch;display:grid;gap:14px;margin-bottom:18px}.identity{font:12px/1.4 var(--mono);color:var(--muted);letter-spacing:.05em}
h1{font:600 clamp(26px,3vw,39px)/1.16 var(--mono);letter-spacing:-.04em;text-wrap:balance;max-width:42ch;margin:0}h2{font:600 23px/1.25 var(--mono);letter-spacing:-.035em;margin:0;scroll-margin-top:calc(var(--toolbar-height) + 15px)}
p,ol,ul{margin:0}a{color:var(--link);text-underline-offset:3px}a:hover{text-decoration-thickness:2px}:focus-visible{outline:3px solid var(--next);outline-offset:3px}
code{font: .91em/1.4 var(--mono);overflow-wrap:anywhere}strong{font-weight:650}.hook{text-decoration:none}.hook:hover{text-decoration:underline}.hook code{color:var(--link)}
nav{display:flex;flex-wrap:wrap;gap:8px 18px;border-top:1px solid var(--line);padding:14px 0 20px;font:12px/1.45 var(--mono)}summary{cursor:pointer;color:var(--muted);font:13px/1.5 var(--mono)}details>p{margin-top:12px}.jump{margin-bottom:14px}.jump>summary{padding:7px 0}
.controls{position:sticky;top:0;z-index:30;display:flex;flex-wrap:wrap;align-items:center;gap:12px 20px;padding:12px 0;background:var(--paper);border-top:1px solid var(--line);border-bottom:1px solid var(--line)}
label{display:flex;gap:9px;align-items:center;font:13px var(--mono)}input,select,button{font:14px var(--body);color:var(--ink);background:var(--surface);border:1px solid #648092;border-radius:3px;min-height:40px;padding:8px 10px}input{width:min(36vw,410px)}input::placeholder{color:var(--muted);opacity:1}button{cursor:pointer}#match-count{margin-left:auto;color:var(--muted);font:12px var(--mono)}
section{margin-top:26px;display:grid;gap:15px;min-width:0}table{border-collapse:separate;border-spacing:0;width:100%;table-layout:fixed;background:var(--surface);font-size:14px;line-height:1.45;border:1px solid var(--line)}
.event-col{width:16%}.next-col{width:25%}thead th{position:sticky;top:var(--toolbar-height);z-index:15;text-align:left;font:600 12px/1.4 var(--mono);background:#213443;color:var(--ink);padding:13px 12px;border-right:1px solid var(--line);border-bottom:2px solid #648092}
tbody th,td{vertical-align:top;text-align:left;padding:15px 12px;border-right:1px solid var(--line);border-bottom:1px solid var(--line);overflow-wrap:normal}tbody th{font-weight:600;background:#14232f;font-size:15px}td:last-child,thead th:last-child{border-right:0}tbody tr:last-child>*{border-bottom:0}tr:hover td{background:#1b2d3b}
.now,.proposal{display:inline-block;font:600 10px/1.4 var(--mono);letter-spacing:.035em;padding:2px 5px;border:1px solid var(--line);margin:6px 5px 3px 0;vertical-align:baseline}.now{color:var(--muted)}.proposal.next{color:var(--next);border-color:#957454}.proposal.keep{color:var(--keep);border-color:#587d69}.proposal.gap{color:var(--gap);border-color:#976d74}.proposal.later,.proposal.skip{color:var(--muted);border-style:dashed}
.notes-section{max-width:110ch;padding:22px 24px;border:1px solid var(--line);background:var(--surface);gap:16px}.notes-section li+li{margin-top:12px}.notes-section p{max-width:100ch}.notes-section code{color:var(--ink)}
#no-matches{padding:28px;color:var(--muted)}footer{margin-top:24px;display:grid;gap:7px;color:var(--muted);font-size:12px}footer code{font-size:11px}[hidden]{display:none!important}
@media(max-width:1050px){main{padding:22px 18px 40px}table{font-size:13px}tbody th,td{padding:12px 9px}thead th{padding:11px 9px}.event-col{width:16%}.next-col{width:25%}}
@media(max-width:760px){main{padding:20px 14px 36px}header{font-size:15px;gap:12px}h1{font-size:27px}h2{font-size:21px}nav{gap:10px 15px}.controls{gap:9px;padding:10px 0}label{font-size:12px}label:first-child{width:100%}input{width:auto;flex:1;min-width:0}select{max-width:150px}#match-count{width:100%;margin:0;font-size:11px}section{margin-top:22px}table,tbody,tr,td,tbody th{display:block;width:100%}colgroup,thead{display:none}table{border:0;background:transparent}tr{border:1px solid var(--line);margin-bottom:17px;background:var(--surface)}tbody th,td{border-right:0;border-bottom:1px solid var(--line);padding:13px 14px;font-size:15px}tbody th{font-size:17px}td:last-child{border-bottom:0}td::before{content:attr(data-label);display:block;color:var(--muted);font:11px/1.4 var(--mono);margin-bottom:7px}.notes-section{padding:18px 16px}footer code{overflow-wrap:anywhere}}
@media print{.controls,nav{display:none}thead th{position:static}body{font-size:12px}main{padding:0}table{font-size:10px}tr{break-inside:avoid}section{break-inside:auto}}
</style></head><body><main>
<header><div class="identity">WEZTERM-ATTENTION / LIFECYCLE CONTRACT MAP / ${escape(catalog.checked_at)}</div>${intro}</header>
<details class="jump"><summary>Jump to a lifecycle group or notes</summary><nav aria-label="Lifecycle groups">${nav}</nav></details>
<div class="controls"><label>Find <input id="query" type="search" placeholder="Lifecycle event or hook name" autocomplete="off"></label>
<label>Proposal <select id="action"><option value="all">All rows</option><option value="next">Next</option><option value="later">Later</option><option value="keep">Keep</option><option value="skip">Skip</option><option value="gap">Gaps</option></select></label>
<button id="reset" type="button">Reset</button><output id="match-count" aria-live="polite"></output></div>
<p id="no-matches" hidden>No lifecycle rows match. Reset the filters to see the complete map.</p>
${sections}
<footer><span>${rowCount} lifecycle rows. Catalog check: ${coverage.map(c => `${escape(c.provider)} ${c.mapped}/${c.total}`).join(' · ')}. Claude notification types ${foundNotificationTypes.size}/${catalog.providers[0].notification_types.length}.</span>
<span>Generated from <a href="./${escape(basename(sourcePath))}">Markdown</a> and checked against the <a href="./${escape(basename(catalogPath))}">provider catalog</a>. No external assets or changes to attention state.</span>
<code>Markdown SHA-256: ${report.source_sha256}</code></footer>
</main><script>
const rows=[...document.querySelectorAll('.lifecycle-row')];
const groups=[...document.querySelectorAll('.matrix-section')];
const query=document.getElementById('query'), action=document.getElementById('action');
function filter(){
 const term=query.value.trim().toLowerCase();let count=0;
 for(const row of rows){const matches=(!term||row.textContent.toLowerCase().includes(term))&&(action.value==='all'||row.dataset.actions.split(' ').includes(action.value));row.hidden=!matches;if(matches)count++;}
 for(const group of groups)group.hidden=![...group.querySelectorAll('.lifecycle-row')].some(row=>!row.hidden);
 document.getElementById('match-count').textContent='Showing '+count+' of '+rows.length+' lifecycle rows';
 document.getElementById('no-matches').hidden=count!==0;
}
query.addEventListener('input',filter);action.addEventListener('change',filter);
document.getElementById('reset').addEventListener('click',()=>{query.value='';action.value='all';filter();});
document.querySelector('nav').addEventListener('click',event=>{const link=event.target.closest('a');if(!link)return;const target=document.getElementById(link.hash.slice(1));if(target?.closest('section')?.hidden){query.value='';action.value='all';filter();}});
const controls=document.querySelector('.controls');
new ResizeObserver(()=>document.documentElement.style.setProperty('--toolbar-height',controls.getBoundingClientRect().height+'px')).observe(controls);
filter();
</script></body></html>`;
writeFileSync(outputPath, html);
writeFileSync(join(dirname(sourcePath), basename(sourcePath, '.md') + '.checks.json'), JSON.stringify(report, null, 2) + '\n');
console.log(JSON.stringify({html: outputPath, ...report}, null, 2));
