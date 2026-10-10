// Builds the Vyrtel website into docs/build/ (served by GitHub Pages): `npm run build` in docs/.
//
// - site/index.html is the front page; {{PLACEHOLDERS}} are filled from the repo (version from
//   Cargo.toml, repository from $GITHUB_REPOSITORY or the git remote, logo path from the web UI).
// - README.md (as "Overview"), docs/*.md and docs/brand/README.md are rendered to
//   build/docs/<slug>.html, so every doc lives under /docs/ on the site. Links between docs become
//   site links; links to other repo files point at GitHub.
// - Brand assets, favicons, screenshots and fonts are copied from where the repo already keeps them,
//   so the site never drifts from the app.
//
// All URLs are relative, so the site works at https://<owner>.github.io/<repo>/ or any other base.

import { execSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { Marked } from 'marked';
import { getHeadingList, gfmHeadingId } from 'marked-gfm-heading-id';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '..');
const out = path.join(here, 'build');
const read = (p) => fs.readFileSync(path.join(root, p), 'utf8');

// ---------- repository facts ----------

function repository() {
  if (process.env.GITHUB_REPOSITORY) return process.env.GITHUB_REPOSITORY;
  try {
    const remote = execSync('git remote get-url origin', { cwd: root, encoding: 'utf8' }).trim();
    const m = remote.match(/github\.com[:/]([^/]+\/[^/.]+?)(?:\.git)?$/);
    if (m) return m[1];
  } catch {
    // not a git checkout
  }
  return 'rowan-smith/Vyrtel';
}

const repo = repository();
const [owner, name] = repo.split('/');
const repoUrl = `https://github.com/${repo}`;
const siteUrl = (process.env.SITE_URL ?? `https://${owner.toLowerCase()}.github.io/${name}/`).replace(/\/?$/, '/');
const version = read('Cargo.toml').match(/\[workspace\.package\][^[]*?version\s*=\s*"([^"]+)"/)[1];
// Published by .github/workflows/release.yml (ghcr.io/<owner>/<repo>, lowercase); `latest` = newest release.
const image = `ghcr.io/${repo.toLowerCase()}:latest`;
const word = read('web/src/lib/wordmark.ts');
const wordPath = word.match(/path: '([^']+)'/)[1];
const wordWidth = Number(word.match(/width: ([\d.]+)/)[1]);

// ---------- shared chrome ----------

/** The integrated [V]yrtel wordmark, same geometry as the web UI (web/src/App.tsx). */
const brand = (base) => `
<a class="brand is-expanded" href="${base}index.html" title="Vyrtel home">
  <svg class="brand-mark" viewBox="0 0 96 96" aria-hidden="true">
    <path d="M18 18 L47 72" fill="none" stroke="var(--mint)" stroke-width="10" stroke-linecap="round"/>
    <path d="M78 18 L49 72" fill="none" stroke="var(--iris)" stroke-width="10" stroke-linecap="round"/>
    <circle cx="48" cy="73" r="7" fill="var(--brand-node-ring)"/>
    <circle class="brand-node" cx="48" cy="73" r="4" fill="var(--brand-node)"/>
  </svg>
  <span class="brand-word" aria-hidden="true"><svg viewBox="0 -96.88 ${wordWidth} 121" style="width:${wordWidth / 100}em"><path d="${wordPath}" fill="currentColor"/></svg></span>
  <span class="sr-only">Vyrtel</span>
</a>`;

const githubIcon = `<svg viewBox="0 0 24 24" width="18" height="18" aria-hidden="true"><path fill="currentColor" d="M12 .5a11.5 11.5 0 0 0-3.64 22.41c.58.1.79-.25.79-.56v-2c-3.2.7-3.88-1.37-3.88-1.37-.52-1.33-1.28-1.69-1.28-1.69-1.04-.71.08-.7.08-.7 1.15.08 1.76 1.19 1.76 1.19 1.03 1.76 2.69 1.25 3.35.96.1-.75.4-1.25.73-1.54-2.55-.29-5.24-1.28-5.24-5.68 0-1.25.45-2.28 1.18-3.08-.12-.29-.51-1.46.11-3.05 0 0 .97-.31 3.17 1.18a11 11 0 0 1 5.77 0c2.2-1.49 3.17-1.18 3.17-1.18.62 1.59.23 2.76.11 3.05.74.8 1.18 1.83 1.18 3.08 0 4.41-2.69 5.39-5.25 5.67.41.36.78 1.06.78 2.14v3.17c0 .31.21.67.8.56A11.5 11.5 0 0 0 12 .5z"/></svg>`;

const chevron = `<svg class="chevron" viewBox="0 0 16 16" width="14" height="14" aria-hidden="true"><path d="M4 6l4 4 4-4" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"/></svg>`;
const sunIcon = `<svg class="icon-sun" viewBox="0 0 24 24" width="18" height="18" aria-hidden="true"><circle cx="12" cy="12" r="4" fill="none" stroke="currentColor" stroke-width="1.8"/><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"/></svg>`;
const moonIcon = `<svg class="icon-moon" viewBox="0 0 24 24" width="18" height="18" aria-hidden="true"><path d="M20 14.5A8 8 0 0 1 9.5 4a8 8 0 1 0 10.5 10.5z" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round"/></svg>`;

/** Docs pages grouped for the menus: filled in once the docs are parsed (see build()). */
let docNav = [];

/** The grouped list of docs pages, shared by the header dropdown and the docs page picker. */
const docMenu = (base, currentSlug) =>
  `<div class="dropdown-panel">${docNav
    .map(
      (g) => `<div class="dd-group"><h3>${g.group}</h3>${g.pages
        .map((d) => `<a href="${base}docs/${d.slug}.html"${d.slug === currentSlug ? ' aria-current="page"' : ''}>${d.title}</a>`)
        .join('')}</div>`,
    )
    .join('')}</div>`;

const themeToggle = (cls) =>
  `<button class="theme-toggle ${cls}" type="button" data-theme-toggle aria-label="Switch colour theme">${sunIcon}${moonIcon}</button>`;

const header = (base, current, currentSlug) => `
<header class="site-header">
  <div class="wrap header-inner">
    ${brand(base)}
    ${themeToggle('theme-toggle-mobile')}
    <button class="nav-toggle" type="button" aria-expanded="false" aria-controls="site-nav" aria-label="Menu"><span></span><span></span></button>
    <nav id="site-nav" class="site-nav" aria-label="Main">
      <a href="${base}index.html#features"${current === 'features' ? ' aria-current="page"' : ''}>Features</a>
      <details class="dropdown nav-dropdown">
        <summary${current === 'docs' ? ' aria-current="page"' : ''}>Docs ${chevron}</summary>
        ${docMenu(base, currentSlug)}
      </details>
      <a href="${base}index.html#download">Download</a>
      <a href="${repoUrl}/releases">Releases</a>
      <a class="nav-source" href="${repoUrl}">${githubIcon}<span>Source</span></a>
      ${themeToggle('theme-toggle-desktop')}
      <a class="button button-primary nav-cta" href="${base}index.html#download">Get Vyrtel</a>
    </nav>
  </div>
</header>`;

const footer = (base) => `
<footer class="site-footer">
  <div class="wrap footer-inner">
    <div class="footer-brand">
      ${brand(base)}
      <p class="tagline">View. Trace. Understand.</p>
      <p class="muted small">Self-hosted observability in one small binary. MIT licensed.</p>
    </div>
    <nav class="footer-links" aria-label="Footer">
      <div>
        <h2>Product</h2>
        <a href="${base}index.html#features">Features</a>
        <a href="${base}index.html#download">Download</a>
        <a href="${repoUrl}/releases">Release notes</a>
        <a href="${base}docs/roadmap.html">Roadmap</a>
      </div>
      <div>
        <h2>Docs</h2>
        <a href="${base}docs/overview.html">Quick start</a>
        <a href="${base}docs/query-language.html">Query language</a>
        <a href="${base}docs/api.html">HTTP API</a>
        <a href="${base}docs/configuration.html">Configuration</a>
      </div>
      <div>
        <h2>Project</h2>
        <a href="${repoUrl}">Source code</a>
        <a href="${repoUrl}/issues">Issues</a>
        <a href="${base}docs/contributing.html">Contributing</a>
        <a href="${repoUrl}/blob/main/LICENSE">License (MIT)</a>
        <a href="${base}docs/brand.html">Brand guide</a>
      </div>
    </nav>
  </div>
  <div class="wrap footer-base muted small">
    <span>Vyrtel ${version}</span>
    <span>Pronounced VER-tel.</span>
  </div>
</footer>`;

const head = (base, title, description) => `
<meta charset="utf-8">
<script>(function(){var d=document.documentElement,t=null;try{t=localStorage.getItem('vyrtel-theme')}catch(e){}if(t==='light'||t==='dark')d.setAttribute('data-theme',t);d.classList.add('js')})()</script>
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>${title}</title>
<meta name="description" content="${description}">
<meta name="theme-color" content="#F6F8FB">
<meta name="color-scheme" content="light dark">
<meta property="og:type" content="website">
<meta property="og:title" content="${title}">
<meta property="og:description" content="${description}">
<meta property="og:image" content="${siteUrl}assets/og.png">
<meta name="twitter:card" content="summary_large_image">
<link rel="icon" href="${base}favicon.ico" sizes="48x48">
<link rel="icon" type="image/svg+xml" href="${base}favicon.svg">
<link rel="apple-touch-icon" href="${base}apple-touch-icon.png">
<link rel="preload" href="${base}assets/fonts/inter.woff2" as="font" type="font/woff2" crossorigin>
<link rel="stylesheet" href="${base}assets/site.css?v=${assetVersion.css}">
<script src="${base}assets/site.js?v=${assetVersion.js}" defer></script>`;

/**
 * Content fingerprints for the CSS and JS, appended to their URLs (`site.css?v=…`). The host
 * (Cloudflare in front of GitHub Pages) lets browsers keep these files for hours while HTML is
 * always fresh, so without this a returning visitor gets new markup with stale styles.
 */
const assetVersion = { css: 'dev', js: 'dev' };
const fingerprint = (text) => createHash('sha256').update(text).digest('hex').slice(0, 10);

const fill = (s, vars) => s.replace(/\{\{([A-Z_]+)\}\}/g, (m, k) => (k in vars ? vars[k] : m));

// ---------- docs ----------

const DOCS = [
  { slug: 'overview', file: 'README.md', title: 'Overview', group: 'Start here' },
  { slug: 'query-language', file: 'docs/query-language.md', group: 'Using Vyrtel' },
  { slug: 'api', file: 'docs/api.md', group: 'Using Vyrtel' },
  { slug: 'configuration', file: 'docs/configuration.md', group: 'Running Vyrtel' },
  { slug: 'architecture', file: 'docs/architecture.md', group: 'Internals' },
  { slug: 'storage-format', file: 'docs/storage-format.md', group: 'Internals' },
  { slug: 'development', file: 'docs/development.md', group: 'Contributing' },
  { slug: 'testing', file: 'docs/testing.md', group: 'Contributing' },
  { slug: 'contributing', file: 'CONTRIBUTING.md', group: 'Project' },
  { slug: 'cla-individual', file: 'docs/cla/individual.md', title: 'Individual CLA', group: 'Project' },
  { slug: 'cla-corporate', file: 'docs/cla/corporate.md', title: 'Corporate CLA', group: 'Project' },
  { slug: 'roadmap', file: 'docs/roadmap.md', group: 'Project' },
  { slug: 'benchmarks', file: 'docs/benchmarks.md', title: 'Benchmarks', group: 'Project' },
  { slug: 'brand', file: 'docs/brand/README.md', title: 'Brand guide', group: 'Project' },
];
const bySource = new Map(DOCS.map((d) => [d.file, d]));
const copiedDirs = ['docs/screenshots/', 'docs/brand/'];
const ASSET = /\.(png|svg|jpe?g|gif|webp|json)$/i;

let currentSource = '';

/** Rewrites a link or image target written relative to `currentSource` in the repo. */
function rewrite(href) {
  if (!href || /^(https?:|mailto:|#|\/\/)/.test(href)) return href;
  const [p, hash = ''] = href.split('#');
  const frag = hash ? `#${hash}` : '';
  const resolved = path.posix.normalize(path.posix.join(path.posix.dirname(currentSource), p));
  const doc = bySource.get(resolved);
  if (doc) return `${doc.slug}.html${frag}`;
  // Images and data files in these folders are copied next to the pages; anything else (scripts,
  // READMEs) links to GitHub like other repo files.
  const copied = copiedDirs.find((d) => resolved.startsWith(d));
  if (copied && ASSET.test(resolved)) return resolved.slice('docs/'.length);
  const full = path.join(root, resolved);
  const kind = fs.existsSync(full) && fs.statSync(full).isDirectory() ? 'tree' : 'blob';
  return `${repoUrl}/${kind}/main/${resolved}${frag}`;
}

const marked = new Marked({ gfm: true });
marked.use(gfmHeadingId());
marked.use({
  walkTokens(t) {
    if (t.type === 'link' || t.type === 'image') t.href = rewrite(t.href);
  },
  renderer: {
    // Mermaid diagrams are drawn in the browser by assets/mermaid.min.js (loaded only on pages
    // that have one, and re-themed with the site); the source stays readable without JS.
    code({ text, lang }) {
      if (lang !== 'mermaid') return false;
      const esc = text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');
      return `<figure class="diagram"><pre class="mermaid">${esc}</pre></figure>\n`;
    },
  },
});

function docSource(d) {
  let src = read(d.file);
  if (d.file === 'README.md') {
    // The README opens with a centred logo block (GitHub-only <picture> markup); the site has its
    // own header. Start at the first paragraph and give the page a heading.
    src = src.replace(/^<p align="center">[\s\S]*?<\/p>\s*/, '# Overview\n\n');
    src = src.replace('git clone <this repository> vyrtel', `git clone ${repoUrl}.git vyrtel`);
  }
  return src;
}

/** Reads a repo JSON file if present, else null (benchmark results are only committed by CI). */
function readJson(rel) {
  try {
    return JSON.parse(fs.readFileSync(path.join(root, rel), 'utf8'));
  } catch {
    return null;
  }
}

const num = (n, digits = 2) =>
  typeof n === 'number' ? n.toLocaleString('en-US', { minimumFractionDigits: digits, maximumFractionDigits: digits }) : '—';

/** Human byte counts matching the harness' ByteFormat, e.g. 11.9 MB. */
const fmtBytes = (b) => {
  if (typeof b !== 'number' || b <= 0) return '—';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let value = b;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  const digits = unit === 0 ? 0 : value < 10 ? 2 : 1;
  return `${value.toLocaleString('en-US', { minimumFractionDigits: digits, maximumFractionDigits: digits })} ${units[unit]}`;
};

const toMb = (b) => (typeof b === 'number' && b > 0 ? b / (1024 * 1024) : null);

/** Build artefacts and storage footprint for the current run (below the chart). */
function buildFacts(data) {
  const build = data.build;
  const storage = data.storage;
  if (!build && !storage) {
    return '';
  }
  const facts = [];
  if (build?.executableBytes) {
    facts.push(['Executable', `${fmtBytes(build.executableBytes)} (${escapeHtml(build.profile ?? 'release')})`]);
  }
  if (build?.dataDirBytes) {
    facts.push(['Data directory', `${fmtBytes(build.dataDirBytes)} on disk after seeding`]);
  }
  if (storage?.eventCount) {
    facts.push(['Events stored', `${num(storage.eventCount, 0)} events in ${num(storage.segmentCount, 0)} segments`]);
    facts.push(['Bytes stored per event', `${num(storage.bytesStoredPerEvent, 2)} B`]);
    if (typeof storage.compressionRatio === 'number' && storage.compressionRatio > 0) {
      facts.push(['Compression', `${num(storage.compressionRatio, 2)}× (${fmtBytes(storage.rawBytes)} raw → ${fmtBytes(storage.storedBytes)} stored)`]);
    }
    if (typeof storage.indexOverhead === 'number' && storage.indexOverhead > 0) {
      facts.push(['Index overhead', `${num(storage.indexOverhead * 100, 1)}% of segment bytes`]);
    }
  }
  if (facts.length === 0) {
    return '';
  }
  return (
    `<h2 id="build-footprint">Build and footprint</h2>` +
    `<dl class="facts">${facts.map(([k, v]) => `<dt>${escapeHtml(k)}</dt><dd>${v}</dd>`).join('')}</dl>`
  );
}

const escapeHtml = (s) =>
  String(s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;');

const shorten = (s, max = 40) => (s.length > max ? `${s.slice(0, max - 1).trimEnd()}…` : s);

/** Inline-SVG horizontal bar chart (log scale) so the site needs no chart library. */
function barChart(rows) {
  const rowHeight = 26;
  const padTop = 6;
  const labelW = 276;
  const valueW = 104;
  const width = 760;
  const plotW = width - labelW - valueW;
  const height = padTop * 2 + rows.length * rowHeight;
  const scale = (v) => Math.log10(1 + Math.max(Number(v) || 0, 0));
  const max = Math.max(...rows.map((r) => scale(r.value)), 0.0001);

  const body = rows
    .map((r, i) => {
      const cy = padTop + i * rowHeight + rowHeight / 2;
      const w = Math.max(1.5, (scale(r.value) / max) * plotW);
      let range = '';
      if (Number.isFinite(r.min) && Number.isFinite(r.max) && r.max > r.min) {
        const x1 = labelW + (scale(r.min) / max) * plotW;
        const x2 = labelW + (scale(r.max) / max) * plotW;
        range = `<line class="chart-range" x1="${x1.toFixed(1)}" y1="${cy}" x2="${x2.toFixed(1)}" y2="${cy}"></line>`;
      }
      return (
        `<text class="chart-label" x="${labelW - 10}" y="${cy + 4}" text-anchor="end">${escapeHtml(shorten(r.label))}</text>` +
        range +
        `<rect class="chart-bar" x="${labelW}" y="${(cy - 7).toFixed(1)}" width="${w.toFixed(1)}" height="14" rx="3"></rect>` +
        `<text class="chart-value" x="${(labelW + w + 8).toFixed(1)}" y="${cy + 4}">${escapeHtml(r.display)}</text>`
      );
    })
    .join('');

  return `<svg class="chart" viewBox="0 0 ${width} ${height}" role="img">${body}</svg>`;
}

function chartFigure(title, caption, rows) {
  if (rows.length === 0) {
    return '';
  }
  return `<figure class="chart-figure"><figcaption>${escapeHtml(title)} <span class="muted">${escapeHtml(caption)}</span></figcaption>${barChart(rows)}</figure>`;
}

/** Inline-SVG line chart of one metric across released versions (dots + connecting line). */
function lineChart(points) {
  const width = 760;
  const height = 220;
  const padTop = 16;
  const padBottom = 34;
  const padLeft = 56;
  const padRight = 24;
  const plotW = width - padLeft - padRight;
  const plotH = height - padTop - padBottom;
  const max = Math.max(...points.map((p) => p.value), 0.0001);
  const x = (i) => (points.length === 1 ? padLeft + plotW / 2 : padLeft + (i / (points.length - 1)) * plotW);
  const y = (v) => padTop + plotH - (v / max) * plotH;
  const fmt = (v) => (v >= 1000 ? Math.round(v / 100) / 10 + 'k' : v >= 10 ? Math.round(v) : Math.round(v * 100) / 100);

  const grid = [0, 0.25, 0.5, 0.75, 1]
    .map((t) => {
      const gy = padTop + plotH - t * plotH;
      return (
        `<line class="chart-axis" x1="${padLeft}" y1="${gy.toFixed(1)}" x2="${(padLeft + plotW).toFixed(1)}" y2="${gy.toFixed(1)}"></line>` +
        `<text class="chart-tick" x="${padLeft - 8}" y="${(gy + 4).toFixed(1)}" text-anchor="end">${fmt(max * t)}</text>`
      );
    })
    .join('');

  const lines =
    points.length > 1
      ? `<polyline class="chart-line" points="${points.map((p, i) => `${x(i).toFixed(1)},${y(p.value).toFixed(1)}`).join(' ')}"></polyline>`
      : '';

  const dots = points
    .map((p, i) => {
      const cx = x(i).toFixed(1);
      const cy = y(p.value).toFixed(1);
      return (
        `<circle class="chart-dot" cx="${cx}" cy="${cy}" r="4"></circle>` +
        `<text class="chart-value" x="${x(i).toFixed(1)}" y="${(y(p.value) - 9).toFixed(1)}" text-anchor="middle">${escapeHtml(p.display)}</text>` +
        `<text class="chart-tick" x="${x(i).toFixed(1)}" y="${(padTop + plotH + 20).toFixed(1)}" text-anchor="middle">${escapeHtml(p.label)}</text>`
      );
    })
    .join('');

  return `<svg class="chart" viewBox="0 0 ${width} ${height}" role="img">${grid}${lines}${dots}</svg>`;
}

/** Reads docs/benchmarks/versions/*.json (one snapshot per released tag), sorted oldest → newest. */
function versionSnapshots() {
  const dir = path.join(root, 'docs', 'benchmarks', 'versions');
  let files = [];
  try {
    files = fs.readdirSync(dir).filter((f) => f.endsWith('.json'));
  } catch {
    return [];
  }
  const parse = (s) => {
    const m = String(s).match(/(\d+)\.(\d+)\.(\d+)/);
    return m ? [Number(m[1]), Number(m[2]), Number(m[3])] : [0, 0, 0];
  };
  return files
    .map((f) => {
      const data = readJson(`docs/benchmarks/versions/${f}`);
      if (!data || !Array.isArray(data.benchmarks) || data.benchmarks.length === 0) return null;
      const label = data.label || data.vyrtelVersion || f.replace(/\.json$/, '');
      return { ...data, label };
    })
    .filter(Boolean)
    .sort((a, b) => {
      const av = parse(a.label);
      const bv = parse(b.label);
      return av[0] - bv[0] || av[1] - bv[1] || av[2] - bv[2];
    });
}

/** Renders the per-version comparison (chart per headline metric + full table) for released tags. */
function versionHistory(snapshots) {
  if (snapshots.length === 0) {
    return '';
  }
  const labels = snapshots.map((s) => s.label);
  const maps = snapshots.map((s) => new Map(s.benchmarks.map((b) => [b.id, b])));
  const newest = snapshots[snapshots.length - 1];

  const order = [];
  const seen = new Set();
  for (const b of [...newest.benchmarks, ...snapshots.flatMap((s) => s.benchmarks)]) {
    if (!seen.has(b.id)) {
      order.push(b);
      seen.add(b.id);
    }
  }

  const cell = (b) => {
    if (!b) return '—';
    if (b.metric === 'throughput') return `${num(b.value, 0)} ${escapeHtml(b.unit)}`;
    return `${num(b.meanMs, b.meanMs < 1 ? 3 : b.meanMs < 100 ? 2 : 0)} ms`;
  };

  // Headline metrics for the version charts: each returns { value, display } per snapshot.
  const byId = (s, id) => s.benchmarks.find((x) => x.id === id);
  const msCell = (b) => `${num(b.meanMs, b.meanMs < 1 ? 3 : b.meanMs < 100 ? 2 : 0)} ms`;
  const throughput = (s, id) => {
    const b = byId(s, id);
    return b ? { value: b.value, display: `${num(b.value, 0)} ${b.unit}` } : null;
  };
  const latency = (s, id) => {
    const b = byId(s, id);
    return b ? { value: b.meanMs, display: msCell(b) } : null;
  };

  const headline = [
    { label: 'Ingest NDJSON', better: 'higher', get: (s) => throughput(s, 'IngestBatch') },
    { label: 'Ingest JSON', better: 'higher', get: (s) => throughput(s, 'IngestBatchJson') },
    { label: 'Latest logs', better: 'lower', get: (s) => latency(s, 'LogsLatest') },
    { label: 'Full-text scan', better: 'lower', get: (s) => latency(s, 'LogsMessageContains') },
    {
      label: 'Executable size',
      better: 'lower',
      get: (s) => {
        const mb = toMb(s.build?.executableBytes);
        return mb ? { value: mb, display: `${num(mb, 2)} MB` } : null;
      },
    },
    {
      label: 'Bytes stored per event',
      better: 'lower',
      get: (s) =>
        s.storage?.eventCount ? { value: s.storage.bytesStoredPerEvent, display: `${num(s.storage.bytesStoredPerEvent, 2)} B` } : null,
    },
  ];

  const figures = headline
    .map(({ label, better, get }) => {
      const points = snapshots
        .map((s) => {
          const p = get(s);
          return p ? { label: s.label, ...p } : null;
        })
        .filter(Boolean);
      if (points.length === 0) return '';
      const caption = points.length === 1 ? 'first released snapshot' : `${better} is better`;
      return `<figure class="chart-figure"><figcaption>${escapeHtml(label)} <span class="muted">${escapeHtml(caption)}</span></figcaption>${lineChart(points)}</figure>`;
    })
    .join('');

  // Extra rows in the comparison table that aren't benchmark rows.
  const extras = [
    { name: 'Executable size', display: (s) => fmtBytes(s.build?.executableBytes) },
    { name: 'Data directory on disk', display: (s) => fmtBytes(s.build?.dataDirBytes) },
    {
      name: 'Bytes stored per event',
      display: (s) => (s.storage?.eventCount ? `${num(s.storage.bytesStoredPerEvent, 2)} B` : '—'),
    },
    {
      name: 'Compression ratio',
      display: (s) => (s.storage?.compressionRatio ? `${num(s.storage.compressionRatio, 2)}×` : '—'),
    },
  ];

  const extraRows = extras
    .map((e) => `<tr class="row-extra"><td>${escapeHtml(e.name)}</td>${snapshots.map((s) => `<td>${e.display(s)}</td>`).join('')}</tr>`)
    .join('');

  const rows = `${order
    .map((sample) => `<tr><td>${escapeHtml(sample.name)}</td>${maps.map((m) => `<td>${cell(m.get(sample.id))}</td>`).join('')}</tr>`)
    .join('')}${extraRows}`;
  const table = `<table><thead><tr><th>Benchmark</th>${labels.map((l) => `<th>${escapeHtml(l)}</th>`).join('')}</tr></thead><tbody>${rows}</tbody></table>`;

  return (
    `<h2 id="version-history">Version history</h2>` +
    `<p class="muted">A snapshot is captured for every released tag (<code>v*</code>), so progress is visible between versions. Older versions are on the left; each column in the table is a full benchmark run.</p>` +
    `${figures}${table}`
  );
}


/** Renders the benchmark rollup injected into docs/benchmarks.md from docs/benchmarks/results.json. */
function benchmarkResults() {
  const data = readJson('docs/benchmarks/results.json');
  if (!data || !Array.isArray(data.benchmarks) || data.benchmarks.length === 0) {
    return `<p class="muted">No benchmark results have been published yet. They are regenerated automatically on the CI machine whenever <code>main</code> changes — see the <a href="${repoUrl}/actions/workflows/benchmark.yml">benchmark workflow</a>.</p>`;
  }

  const meta = [
    `Vyrtel ${data.vyrtelVersion ?? '?'}`,
    `${data.job ?? 'short'} job`,
    data.dataset?.events ? `${data.dataset.events.toLocaleString('en-US')} events` : null,
    data.generatedAt ? `generated ${new Date(data.generatedAt).toISOString().slice(0, 10)}` : null,
  ]
    .filter(Boolean)
    .join(' · ');

  const hw = data.hardware ?? {};
  const machine = [
    hw.os,
    hw.logicalCores ? `${hw.logicalCores} logical cores` : null,
    hw.runtime,
    hw.benchmarkDotNet ? `BenchmarkDotNet ${hw.benchmarkDotNet}` : null,
  ]
    .filter(Boolean)
    .join('; ');

  const groups = [...new Set(data.benchmarks.map((b) => b.group))];
  const byGroup = (g) => data.benchmarks.filter((b) => b.group === g);

  const latencyRows = (list) =>
    list
      .map((b) => ({
        label: b.name,
        value: b.meanMs,
        display: `${num(b.meanMs, b.meanMs < 1 ? 3 : b.meanMs < 100 ? 2 : 0)} ms`,
        min: b.minMs,
        max: b.maxMs,
      }))
      .sort((a, b) => b.value - a.value);

  const throughputRows = (list) =>
    list
      .map((b) => ({ label: `${b.name} (${b.unit})`, value: b.value, display: `${num(b.value, 0)} ${b.unit}` }))
      .sort((a, b) => b.value - a.value);

  const charts = [];
  for (const group of groups) {
    const list = byGroup(group);
    charts.push(
      chartFigure(
        `${group} latency`,
        'mean ms per request · log scale · whisker = min–max · shorter is better',
        latencyRows(list),
      ),
    );
    if (group === 'Query') {
      charts.push(
        chartFigure(
          'Query throughput',
          'requests per second · log scale · longer is better',
          list
            .map((b) => ({ label: b.name, value: b.throughput ?? 0, display: `${num(b.throughput, 0)} ops/s` }))
            .sort((a, b) => b.value - a.value),
        ),
      );
    } else {
      charts.push(
        chartFigure(
          `${group} throughput`,
          'per-series throughput · log scale · unit shown per row · longer is better',
          throughputRows(list),
        ),
      );
    }
  }

  const tables = groups
    .map((group) => {
      const rows = data.benchmarks
        .filter((b) => b.group === group)
        .sort((a, b) => a.name.localeCompare(b.name))
        .map((b) => {
          const speed = b.metric === 'throughput' ? `${num(b.value, 0)} ${b.unit}` : `${num(b.throughput, 0)} ${b.throughputUnit ?? ''}`.trim();
          return `<tr><td>${b.name}</td><td>${num(b.meanMs, 3)} ms</td><td>${num(b.medianMs, 3)} ms</td><td>${num(b.minMs, 3)} ms</td><td>${num(b.maxMs, 3)} ms</td><td>${speed}</td></tr>`;
        })
        .join('');
      return `<h3>${group}</h3><table><thead><tr><th>Benchmark</th><th>Mean</th><th>Median</th><th>Min</th><th>Max</th><th>Throughput</th></tr></thead><tbody>${rows}</tbody></table>`;
    })
    .join('');

  return `<p class="muted">${meta}</p><p class="muted small">Measured on ${machine}.</p>${buildFacts(data)}${charts.join('')}${tables}${versionHistory(versionSnapshots())}`;
}

function parseDocs() {
  return DOCS.map((d) => {
    currentSource = d.file;
    let html = marked.parse(docSource(d));
    if (d.slug === 'benchmarks') {
      html = html.replace('<!--BENCHMARK_RESULTS-->', benchmarkResults());
    }
    const headings = getHeadingList();
    const h1 = headings.find((h) => h.level === 1);
    const title = d.title ?? h1?.raw ?? d.slug;
    return { ...d, title, html, toc: headings.filter((h) => h.level === 2) };
  });
}

function renderDocs(rendered) {
  const template = fs.readFileSync(path.join(here, 'site/doc.html'), 'utf8');
  fs.mkdirSync(path.join(out, 'docs'), { recursive: true });

  rendered.forEach((d, i) => {
    // The docs navigation is a dropdown ("page picker") rather than a full sidebar.
    const picker = `<details class="dropdown doc-picker"><summary><span class="doc-picker-group">${d.group}</span><span class="doc-picker-title">${d.title}</span>${chevron}</summary>${docMenu('../', d.slug)}</details>`;
    const toc = d.toc.length
      ? `<nav class="toc" aria-label="On this page"><h2>On this page</h2>${d.toc.map((h) => `<a href="#${h.id}">${h.text}</a>`).join('')}</nav>`
      : '';
    const prev = rendered[i - 1];
    const next = rendered[i + 1];
    const pager = `<nav class="pager" aria-label="Pages">${prev ? `<a class="prev" href="${prev.slug}.html"><span>Previous</span>${prev.title}</a>` : '<span></span>'}${next ? `<a class="next" href="${next.slug}.html"><span>Next</span>${next.title}</a>` : ''}</nav>`;
    const page = fill(template, {
      HEAD: head('../', `${d.title} · Vyrtel docs`, `Vyrtel documentation: ${d.title}.`),
      HEADER: header('../', 'docs', d.slug),
      FOOTER: footer('../'),
      PICKER: picker,
      TOC: toc,
      CONTENT: d.html,
      PAGER: pager,
      EDIT_URL: `${repoUrl}/edit/main/${d.file}`,
    });
    fs.writeFileSync(path.join(out, 'docs', `${d.slug}.html`), page);
  });
  // /docs/ itself opens the first page.
  fs.writeFileSync(
    path.join(out, 'docs', 'index.html'),
    `<!doctype html><meta charset="utf-8"><title>Vyrtel docs</title><meta http-equiv="refresh" content="0; url=${rendered[0].slug}.html"><link rel="canonical" href="${rendered[0].slug}.html"><a href="${rendered[0].slug}.html">Vyrtel docs</a>`,
  );
  return rendered.length;
}

// ---------- assets ----------

function copy(from, to, filter) {
  fs.mkdirSync(path.dirname(path.join(out, to)), { recursive: true });
  fs.cpSync(path.isAbsolute(from) ? from : path.join(root, from), path.join(out, to), { recursive: true, filter });
}

function build() {
  fs.rmSync(out, { recursive: true, force: true });
  fs.mkdirSync(out, { recursive: true });

  const docs = parseDocs();
  docNav = [...new Set(docs.map((d) => d.group))].map((group) => ({ group, pages: docs.filter((d) => d.group === group) }));

  const vars = {
    REPO: repo,
    REPO_URL: repoUrl,
    SITE_URL: siteUrl,
    VERSION: version,
    IMAGE: image,
    MERMAID_VERSION: JSON.parse(fs.readFileSync(path.join(here, 'node_modules/mermaid/package.json'), 'utf8')).version,
  };
  // Fingerprint the assets first: every page's <head> links to them by content hash.
  const css = fs.readFileSync(path.join(here, 'site/site.css'), 'utf8');
  const js = fill(fs.readFileSync(path.join(here, 'site/site.js'), 'utf8'), vars);
  assetVersion.css = fingerprint(css);
  assetVersion.js = fingerprint(js);

  const index = fill(fs.readFileSync(path.join(here, 'site/index.html'), 'utf8'), {
    ...vars,
    HEAD: head('', 'Vyrtel: View. Trace. Understand.', 'Developer-friendly observability: launch one ~11 MB binary and send logs, traces and metrics. Small, fast, light and full featured.'),
    HEADER: header('', 'home'),
    FOOTER: footer(''),
  });
  fs.writeFileSync(path.join(out, 'index.html'), index);
  fs.mkdirSync(path.join(out, 'assets'), { recursive: true });
  fs.writeFileSync(path.join(out, 'assets/site.css'), css);
  fs.writeFileSync(path.join(out, 'assets/site.js'), js);
  // GitHub Pages: serve files as-is (no Jekyll processing).
  fs.writeFileSync(path.join(out, '.nojekyll'), '');

  for (const f of ['favicon.svg', 'favicon.ico', 'apple-touch-icon.png']) copy(`web/public/${f}`, f);
  copy('docs/brand/vyrtel-og-1200x630.png', 'assets/og.png');
  copy('docs/screenshots', 'assets/screenshots');
  copy('docs/screenshots', 'docs/screenshots');
  copy('docs/brand', 'docs/brand', (f) => fs.statSync(f).isDirectory() || ASSET.test(f));
  const fonts = path.join(here, 'node_modules');
  copy(path.join(fonts, '@fontsource-variable/inter/files/inter-latin-wght-normal.woff2'), 'assets/fonts/inter.woff2');
  copy(path.join(fonts, '@fontsource/ibm-plex-mono/files/ibm-plex-mono-latin-400-normal.woff2'), 'assets/fonts/plex-mono-400.woff2');
  copy(path.join(fonts, '@fontsource/ibm-plex-mono/files/ibm-plex-mono-latin-500-normal.woff2'), 'assets/fonts/plex-mono-500.woff2');

  copy(path.join(here, 'node_modules/mermaid/dist/mermaid.min.js'), 'assets/mermaid.min.js');

  const n = renderDocs(docs);
  console.log(`Built ${path.relative(root, out)}: front page + ${n} docs pages for ${repo} (v${version}), site URL ${siteUrl}`);
}

build();
