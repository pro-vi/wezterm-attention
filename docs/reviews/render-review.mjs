// Render either review from its Markdown source. The eight-choice explainer is the default.
import { readFileSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { resolve, basename } from 'node:path';

const sourceUrl = process.argv[2]
  ? pathToFileURL(resolve(process.argv[2]))
  : new URL('./2026-09-05-attention-v2-eight-choices.md', import.meta.url);
if (!sourceUrl.pathname.endsWith('.md')) throw new Error('Source must be Markdown');
const outputUrl = new URL(sourceUrl.href.replace(/\.md$/, '.html'));
const sourceName = basename(fileURLToPath(sourceUrl));
const isEightChoices = sourceName === '2026-09-05-attention-v2-eight-choices.md';
const isModuleSpec = sourceName.endsWith('-modules.md');
const isPlan = sourceName.endsWith('-plan.md');
const source = readFileSync(sourceUrl, 'utf8');
const digest = createHash('sha256').update(source).digest('hex');
const frontmatter = source.match(/^---\r?\n([\s\S]*?)\r?\n---\r?\n/);
let body = Bun.markdown.html(frontmatter ? source.slice(frontmatter[0].length) : source);
const title = body.match(/<h1>(.*?)<\/h1>/s)?.[1];
if (!title) throw new Error('Source must contain a title');

const units = [];
if (isPlan) {
  body = body.replace(/<h3>(U([1-9][0-9]*)\. [\s\S]*?)<\/h3>/g, (_, text, number) => {
    const id = `u${number}`;
    if (units.some(unit => unit.id === id)) throw new Error('Unit IDs must be distinct');
    units.push({ id, text });
    return `<h3 id="${id}">${text}</h3>`;
  });
}

const headings = [];
body = body.replace(/<h2>([\s\S]*?)<\/h2>/g, (_, text) => {
  const plain = text.replace(/<[^>]+>/g, '').trim();
  const question = plain.match(/^Q([1-8])\b/);
  const id = question ? `q${question[1]}` : plain.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');
  if (!id || headings.some(heading => heading.id === id)) throw new Error('Headings must have distinct readable names');
  headings.push({ id, text });
  return `<h2 id="${id}">${text}</h2>`;
});
if (isEightChoices && headings.filter(heading => /^q[1-8]$/.test(heading.id)).length !== 8) {
  throw new Error('Expected all eight source questions');
}
if (isPlan) {
  const anchors = new Set([...headings, ...units].map(item => item.id));
  body = body.replace(/<h3>([\s\S]*?)<\/h3>/g, (_, text) => {
    const plain = text.replace(/<[^>]+>/g, '').trim();
    const stem = plain.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');
    if (!stem) throw new Error('Plan subsection must have a readable anchor');
    let id = stem;
    for (let suffix = 2; anchors.has(id); suffix++) id = `${stem}-${suffix}`;
    anchors.add(id);
    return `<h3 id="${id}">${text}</h3>`;
  });
}
body = body.replace(/<table>([\s\S]*?)<\/table>/g, (_, contents) => {
  const headers = [...contents.matchAll(/<th(?:\s[^>]*)?>([\s\S]*?)<\/th>/g)]
    .map(match => match[1].replace(/<[^>]+>/g, '').trim());
  const kind = headers[0] === 'Question' ? 'facts' : headers[0] === 'Poll' ? 'samples' : 'comparison-table';
  const rows = contents.replace(/<tbody>([\s\S]*?)<\/tbody>/, (_, rows) =>
    '<tbody>' + rows.replace(/<tr>([\s\S]*?)<\/tr>/g, (_, cells) => {
      let index = 0;
      return '<tr>' + cells.replace(/<td>/g, () =>
        `<td data-label="${Bun.escapeHTML(headers[index++] ?? '')}">`) + '</tr>';
    }) + '</tbody>');
  return `<table class="${kind}">${rows}</table>`;
});
body = body.replace(/(<pre>(?:(?!<\/pre>)[\s\S])*<\/pre>)\s*(<pre>(?:(?!<\/pre>)[\s\S])*<\/pre>)/g,
  '<div class="file-comparison">$1$2</div>');
body = body.replace(/<p>(<a href=[\s\S]*?)<\/p>/g, (whole, links) =>
  links.startsWith('<a href=') && links.includes('Source:') ? `<p class="source-link">${links}</p>` : whole);

const sections = body.split(/(?=<h2 id=)/);
const intro = sections.shift();
const sectionMarkup = sections.map(section => {
  const id = section.match(/<h2 id="([^"]+)"/)?.[1];
  return `<section aria-labelledby="${id}">${section}</section>`;
});
const navigation = isEightChoices ? headings.filter(heading => /^q[1-8]$/.test(heading.id)) : headings;
const nav = navigation.map(({ id, text }) => `<a href="#${id}">${text}</a>`).join('\n');
const navigationMarkup = `<nav aria-label="Jump to a section">${nav}</nav>`;
const unitNavigation = units.length
  ? `<nav class="unit-navigation" aria-label="Implementation units">${units.map(({ id, text }) => `<a href="#${id}">${text}</a>`).join('\n')}</nav>`
  : '';
const content = isPlan
  ? `<details class="metadata"><summary>Browse the full plan contract (${headings.length} sections)</summary>${navigationMarkup}</details>` + sectionMarkup.join('\n').replace(/(<h2 id="implementation-units">[\s\S]*?<\/h2>)/, `$1${unitNavigation}`)
  : isModuleSpec
    ? sectionMarkup[0] + navigationMarkup + sectionMarkup.slice(1).join('\n')
    : navigationMarkup + sectionMarkup.join('\n');
const metadata = frontmatter
  ? `<details class="metadata"><summary>Canonical plan metadata</summary><pre>${Bun.escapeHTML(frontmatter[1])}</pre></details>`
  : '';

// Terminal records, callbacks and commands are the page's material.
// Mono headings identify those subjects; sans text explains; mono blocks show instances.
const html = `<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="color-scheme" content="dark">
<meta name="source-sha256" content="${digest}">
<meta name="description" content="${Bun.escapeHTML(title.replace(/<[^>]+>/g, ''))}">
<title>${title} · attention-v2</title>
<style>
:root{color-scheme:dark;--paper:#0e1720;--surface:#16232e;--ink:#e7eef5;--muted:#afc0cf;--line:#3c5161;--link:#91cbf4;--terminal:#0b141c;--terminal-ink:#e0edf7;--terminal-rule:#627d8d;--note:#203240;--focus:#ffc18a;--mono:Menlo,Consolas,monospace;--reading:-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif}
*{box-sizing:border-box}html{scroll-padding-top:24px}body{margin:0;background:var(--paper);color:var(--ink);font:17px/1.6 var(--reading)}
a{color:var(--link);text-underline-offset:3px}a:hover{text-decoration-thickness:2px}a:focus-visible{outline:3px solid var(--focus);outline-offset:4px}
main{max-width:1100px;margin:auto;padding:36px 32px 64px;display:grid;gap:30px}header{display:grid;gap:16px}
.identity{font:12px/1.6 var(--mono);letter-spacing:.04em;color:var(--muted);display:flex;gap:16px;flex-wrap:wrap}
h1,h2{font-family:var(--mono);line-height:1.25;text-wrap:balance;margin:0}h1{font-size:clamp(26px,3.1vw,38px);max-width:27ch;letter-spacing:-.035em}h2{font-size:24px;letter-spacing:-.035em}
h3{font:600 19px/1.4 var(--mono);margin:8px 0 0;text-wrap:balance}li+li{margin-top:7px}
p,pre,table,ul,ol{margin:0}header p{max-width:78ch}header p:first-of-type{font-size:19px;font-weight:550}
nav{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));border-top:1px solid var(--line);border-left:1px solid var(--line)}
nav a{padding:11px 14px;background:var(--surface);border-right:1px solid var(--line);border-bottom:1px solid var(--line);font:13px/1.5 var(--mono);text-decoration:none}
nav a:hover{background:var(--note)}section{background:var(--surface);padding:28px;border:1px solid var(--line);display:grid;gap:18px;min-width:0}
.unit-navigation{grid-template-columns:repeat(2,minmax(0,1fr))}.unit-navigation a{font-size:14px}.metadata{font-size:14px;min-width:0}.metadata summary{cursor:pointer;color:var(--muted);padding:8px 0}.metadata pre{margin-top:12px}summary:focus-visible{outline:3px solid var(--focus);outline-offset:4px}
section>p{max-width:78ch}section>h2+p,section>blockquote+p{font-size:19px;line-height:1.5}strong{font-weight:650}
blockquote{margin:0;padding:14px 18px;background:var(--note);color:var(--muted);max-width:82ch}blockquote strong{font:12px/1.5 var(--mono);color:var(--link)}
pre{padding:19px 20px;background:var(--terminal);color:var(--terminal-ink);font:14px/1.65 var(--mono);white-space:pre-wrap;overflow-wrap:anywhere;border-top:5px solid var(--terminal-rule);min-width:0}
pre code{font:inherit;background:none;padding:0;color:inherit}code{font-family:var(--mono);font-size:.88em;overflow-wrap:anywhere}
.file-comparison{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:14px;align-items:stretch;min-width:0}
table{border-collapse:collapse;width:100%;font-size:16px;text-align:left}th,td{padding:12px 14px;border-bottom:1px solid var(--line);vertical-align:top}th{font-family:var(--mono);font-size:12px;background:var(--note);letter-spacing:.03em}td{overflow-wrap:anywhere}
.facts th:first-child,.facts td:first-child{width:24%}.facts td:first-child{font-weight:600}.samples th:first-child,.samples td:first-child{width:12%}.samples td{font-variant-numeric:tabular-nums}.samples td:last-child{font-family:var(--mono);font-weight:600}
.comparison-table th:first-child{width:25%}.source-link{font-size:13px;line-height:1.5;color:var(--muted)}
footer{display:grid;gap:5px;color:var(--muted);font-size:12px}footer code{font-size:11px}section[aria-labelledby="review-state"]{background:var(--note)}
@media(max-width:780px){main{padding:22px 16px 36px;gap:20px}section{padding:20px;gap:16px}h2{font-size:20px}nav{grid-template-columns:1fr}.file-comparison{grid-template-columns:1fr}pre{font-size:13px;padding:14px 12px}.facts thead,.comparison-table thead{display:none}.facts,.facts tbody,.facts tr,.facts td,.comparison-table,.comparison-table tbody,.comparison-table tr,.comparison-table td{display:block}.facts tr,.comparison-table tr{padding:12px 0;border-bottom:1px solid var(--line)}.facts td,.comparison-table td{border:0;padding:3px 0}.facts td:first-child{width:auto;font-size:14px;color:var(--muted)}.comparison-table td::before{content:attr(data-label);display:block;font:12px/1.5 var(--mono);color:var(--muted);margin-top:7px}.samples th,.samples td{padding:9px 6px}.samples{font-size:14px}section>h2+p,section>blockquote+p{font-size:18px}}
@media print{:root{color-scheme:light;--paper:#fff;--surface:#fff;--ink:#172a38;--muted:#506171;--line:#c9d5dd;--link:#185e8a;--terminal:#edf2f5;--terminal-ink:#172a38;--note:#e7eff5}main{padding:0;max-width:none}nav{display:none}section{break-inside:avoid}a{color:inherit}}
@media(max-width:780px){.unit-navigation{grid-template-columns:1fr}}
</style></head><body><main>
<header><div class="identity"><span>wezterm-attention / ${isPlan ? 'architecture proposal' : 'design review'}</span><span>Source-backed examples · no live changes</span></div>${intro}</header>
${content}
${metadata}
<footer><a href="./${Bun.escapeHTML(sourceName)}">Read the Markdown source</a><span>Generated offline from that source. No external fonts, scripts or images.</span><code>Source SHA-256: ${digest}</code></footer>
</main></body></html>`;
writeFileSync(outputUrl, html);
console.log(JSON.stringify({ html: fileURLToPath(outputUrl), source_sha256: digest, sections: headings.length }));
