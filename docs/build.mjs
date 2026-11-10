// Builds the Vyrtel website into docs/build/ (served by GitHub Pages): `npm run build` in docs/.
//
// - site/index.html is the front page; {{PLACEHOLDERS}} are filled from the repo (version from
//   Cargo.toml, repository from $GITHUB_REPOSITORY or the git remote, logo path from the web UI).
// - The docs (docs/guide/*.md, docs/*.md, CONTRIBUTING.md, docs/brand/README.md) are rendered once
//   per documentation version: `latest` from this checkout and every release from its git tag, at
//   build/docs/<version>/<slug>.html, with the newest release also at build/docs/<slug>.html. Links
//   between docs become site links; links to other repo files point at GitHub at that version.
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

/** The benchmark pages, which the main nav's Benchmarks link covers instead of Docs. */
const isBenchmarkPage = (slug) => slug === 'benchmarks' || slug === 'benchmark-method';

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
      <a href="${base}docs/index.html"${current === 'docs' && !isBenchmarkPage(currentSlug) ? ' aria-current="page"' : ''}>Docs</a>
      <a href="${base}docs/benchmarks.html"${isBenchmarkPage(currentSlug) ? ' aria-current="page"' : ''}>Benchmarks</a>
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
        <a href="${base}docs/benchmarks.html">Benchmarks</a>
        <a href="${base}docs/roadmap.html">Roadmap</a>
      </div>
      <div>
        <h2>Docs</h2>
        <a href="${base}docs/quickstart.html">Quickstart</a>
        <a href="${base}docs/opentelemetry.html">OpenTelemetry</a>
        <a href="${base}docs/query-language.html">Query language</a>
        <a href="${base}docs/configuration.html">Configuration</a>
        <a href="${base}docs/api.html">HTTP API</a>
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

/**
 * Every docs page. Pages outside the Project group are versioned: each documentation version renders
 * them from its own git ref, and a version simply lacks the pages its ref does not have. Project
 * pages (roadmap, benchmarks, contributing...) describe the project rather than a release, so every
 * version shows them from the current checkout.
 */
const DOCS = [
  { slug: 'introduction', file: 'docs/guide/introduction.md', group: 'Get started' },
  { slug: 'quickstart', file: 'docs/guide/quickstart.md', group: 'Get started' },
  { slug: 'installation', file: 'docs/guide/installation.md', group: 'Get started' },
  { slug: 'opentelemetry', file: 'docs/guide/opentelemetry.md', title: 'OpenTelemetry', group: 'Send data' },
  { slug: 'http-ingestion', file: 'docs/guide/http-ingestion.md', title: 'HTTP and JSON', group: 'Send data' },
  { slug: 'query-language', file: 'docs/query-language.md', group: 'Search' },
  { slug: 'docker', file: 'docs/guide/docker.md', title: 'Docker', group: 'Operate' },
  { slug: 'security', file: 'docs/guide/security.md', group: 'Operate' },
  { slug: 'backups-and-upgrades', file: 'docs/guide/backups-and-upgrades.md', group: 'Operate' },
  { slug: 'configuration', file: 'docs/configuration.md', group: 'Reference' },
  { slug: 'api', file: 'docs/api.md', group: 'Reference' },
  { slug: 'architecture', file: 'docs/architecture.md', group: 'How it works' },
  { slug: 'storage-format', file: 'docs/storage-format.md', group: 'How it works' },
  { slug: 'benchmarks', file: 'docs/benchmarks.md', title: 'Benchmarks', group: 'Project' },
  { slug: 'benchmark-method', file: 'docs/benchmark-method.md', title: 'Benchmark method', group: 'Project' },
  { slug: 'roadmap', file: 'docs/roadmap.md', group: 'Project' },
  { slug: 'contributing', file: 'CONTRIBUTING.md', title: 'Contributing', group: 'Project' },
  { slug: 'development', file: 'docs/development.md', group: 'Project' },
  { slug: 'brand', file: 'docs/brand/README.md', title: 'Brand guide', group: 'Project' },
  // Rendered and linked from other pages, but not listed in the navigation.
  { slug: 'cla-individual', file: 'docs/cla/individual.md', title: 'Individual CLA', group: 'Project', hidden: true },
  { slug: 'cla-corporate', file: 'docs/cla/corporate.md', title: 'Corporate CLA', group: 'Project', hidden: true },
];
const isProjectPage = (d) => d.group === 'Project';

/** Releases before the guides existed opened with the README; it stands in for the introduction. */
const README_OVERVIEW = { slug: 'overview', file: 'README.md', title: 'Overview', group: 'Get started' };

/** Pages that used to exist, so old links still land somewhere useful. Maintainer docs live on GitHub. */
const REDIRECTS = {
  overview: 'introduction.html',
  testing: `${repoUrl}/blob/main/docs/testing.md`,
  releasing: `${repoUrl}/blob/main/docs/releasing.md`,
};

// ---------- versions ----------

/** The oldest release with docs on the site. 0.1.0 shipped under the product's earlier name. */
const OLDEST_DOCS_RELEASE = '0.1.1';

const semver = (v) => v.split('.').map(Number);
const compareVersions = (a, b) => {
  const [x, y] = [semver(a), semver(b)];
  return x[0] - y[0] || x[1] - y[1] || x[2] - y[2];
};

/** Stable release tags (vX.Y.Z, no prereleases) back to OLDEST_DOCS_RELEASE, newest first. */
function releaseVersions() {
  let tags = '';
  try {
    tags = execSync('git tag --list v*', { cwd: root, encoding: 'utf8' });
  } catch {
    return [];
  }
  return tags
    .split('\n')
    .map((t) => t.trim())
    .filter((t) => /^v\d+\.\d+\.\d+$/.test(t))
    .map((t) => t.slice(1))
    .filter((v) => compareVersions(v, OLDEST_DOCS_RELEASE) >= 0)
    .sort((a, b) => compareVersions(b, a));
}

/** A repo file as it is at `ref`, or in the working tree when `ref` is null; null if it is absent. */
function readAt(ref, file) {
  if (!ref) {
    const full = path.join(root, file);
    return fs.existsSync(full) ? fs.readFileSync(full, 'utf8') : null;
  }
  try {
    return execSync(`git show ${ref}:${file}`, {
      cwd: root,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
      maxBuffer: 64 * 1024 * 1024,
    });
  } catch {
    return null;
  }
}

/**
 * The documentation versions: `latest` (this checkout, i.e. main, with unreleased changes) and every
 * stable release, each rendered under docs/<id>/. The newest release is the default and is also
 * rendered at docs/ itself; `latest` is the default only before the first release.
 */
function docVersions() {
  const releases = releaseVersions();
  const list = [
    { id: 'latest', label: 'Latest', ref: null, released: false },
    ...releases.map((v) => ({ id: v, label: v, ref: `v${v}`, released: true })),
  ];
  for (const v of list) {
    const pages = DOCS.filter((d) => isProjectPage(d) || readAt(v.ref, d.file) !== null).map((d) => ({
      ...d,
      ref: isProjectPage(d) ? null : v.ref,
    }));
    if (!pages.some((d) => d.slug === 'introduction') && readAt(v.ref, README_OVERVIEW.file) !== null) {
      pages.unshift({ ...README_OVERVIEW, ref: v.ref });
    }
    v.pages = pages;
    v.slugs = new Set(pages.map((d) => d.slug));
  }
  const latestRelease = list.find((v) => v.released) ?? null;
  return { list, defaultVersion: latestRelease ?? list[0], latestRelease };
}

// ---------- rendering ----------

const copiedDirs = ['docs/screenshots/', 'docs/brand/'];
const ASSET = /\.(png|svg|jpe?g|gif|webp|json)$/i;

/** What the Markdown renderer is working on: set before each page is parsed. */
const current = { source: '', ref: null, bySource: new Map(), assetPrefix: '', crossToLatest: false };

/** Rewrites a link or image target written relative to `current.source` in the repo. */
function rewrite(href) {
  if (!href || /^(https?:|mailto:|#|\/\/)/.test(href)) return href;
  const [p, hash = ''] = href.split('#');
  const frag = hash ? `#${hash}` : '';
  const resolved = path.posix.normalize(path.posix.join(path.posix.dirname(current.source), p));
  const doc = current.bySource.get(resolved);
  // Project pages are written against main, so in a release's docs their links to versioned pages
  // go to the latest docs, where the sections they name exist.
  if (doc && current.crossToLatest && !isProjectPage(doc)) return `${current.assetPrefix}latest/${doc.slug}.html${frag}`;
  if (doc) return `${doc.slug}.html${frag}`;
  if (current.crossToLatest) {
    const latestDoc = DOCS.find((d) => d.file === resolved && !isProjectPage(d));
    if (latestDoc) return `${current.assetPrefix}latest/${latestDoc.slug}.html${frag}`;
  }
  // Images and data files in these folders are copied to docs/; anything else (scripts, READMEs)
  // links to GitHub at the version's ref, like other repo files.
  const copied = copiedDirs.find((d) => resolved.startsWith(d));
  if (copied && ASSET.test(resolved)) return current.assetPrefix + resolved.slice('docs/'.length);
  const full = path.join(root, resolved);
  const kind = fs.existsSync(full) && fs.statSync(full).isDirectory() ? 'tree' : 'blob';
  return `${repoUrl}/${kind}/${current.ref ?? 'main'}/${resolved}${frag}`;
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
  let src = readAt(d.ref, d.file);
  if (d.file === 'README.md') {
    // The README opens with a centred logo block (GitHub-only <picture> markup); the site has its
    // own header. Start at the first paragraph and give the page a heading.
    src = src.replace(/^<p align="center">[\s\S]*?<\/p>\s*/, '# Overview\n\n');
    src = src.replace('git clone <this repository> vyrtel', `git clone ${repoUrl}.git vyrtel`);
  }
  return src;
}

/** Parses a version's pages for output at a directory `assetPrefix` away from docs/. */
function parseDocs(v, assetPrefix) {
  current.bySource = new Map(v.pages.map((d) => [d.file, d]));
  current.assetPrefix = assetPrefix;
  return v.pages.map((d) => {
    current.source = d.file;
    current.ref = d.ref;
    current.crossToLatest = v.released && isProjectPage(d);
    const html = marked.parse(docSource(d));
    const headings = getHeadingList();
    const h1 = headings.find((h) => h.level === 1);
    const title = d.title ?? h1?.raw ?? d.slug;
    // The first paragraph doubles as the page's meta description.
    const first = html.match(/<p>([\s\S]*?)<\/p>/);
    const description = first ? first[1].replace(/<[^>]+>/g, '').replace(/\s+/g, ' ').trim().slice(0, 200) : null;
    return { ...d, title, html, description, toc: headings.filter((h) => h.level === 2) };
  });
}

/** Listed pages by group, for the sidebar and the mobile page picker. */
const navGroups = (docs) => {
  const listed = docs.filter((d) => !d.hidden);
  return [...new Set(listed.map((d) => d.group))].map((group) => ({ group, pages: listed.filter((d) => d.group === group) }));
};

const pageLinks = (pages, currentSlug) =>
  pages.map((d) => `<a href="${d.slug}.html"${d.slug === currentSlug ? ' aria-current="page"' : ''}>${d.title}</a>`).join('');

const sidebar = (groups, currentSlug) =>
  groups.map((g) => `<div class="side-group"><h2>${g.group}</h2>${pageLinks(g.pages, currentSlug)}</div>`).join('');

const pageMenu = (groups, currentSlug) =>
  `<div class="dropdown-panel">${groups.map((g) => `<div class="dd-group"><h3>${g.group}</h3>${pageLinks(g.pages, currentSlug)}</div>`).join('')}</div>`;

const redirectPage = (to, title) =>
  `<!doctype html><meta charset="utf-8"><title>${title}</title><meta name="robots" content="noindex"><meta http-equiv="refresh" content="0; url=${to}"><link rel="canonical" href="${to}"><a href="${to}">${title}</a>`;

/**
 * Renders one version into `dir` (docs/ or docs/<id>/). `toDocs` is the relative path from that
 * directory back to docs/, so links to other versions and the default docs resolve from either.
 */
function renderVersion(versions, v, dir, toDocs) {
  const template = fs.readFileSync(path.join(here, 'site/doc.html'), 'utf8');
  const base = `${toDocs}../`;
  const docs = parseDocs(v, toDocs);
  const groups = navGroups(docs);
  const listed = docs.filter((d) => !d.hidden);
  const { defaultVersion, latestRelease } = versions;
  // Link to `slug` in another version: the same page if it has one, else that version's first page.
  const inVersion = (other, slug) => {
    const target = other.slugs.has(slug) ? slug : other.pages[0].slug;
    return `${toDocs}${other.id}/${target}.html`;
  };
  fs.mkdirSync(path.join(out, dir), { recursive: true });

  for (const d of docs) {
    const picker = `<details class="dropdown doc-picker"><summary><span class="doc-picker-group">${d.group}</span><span class="doc-picker-title">${d.title}</span>${chevron}</summary>${pageMenu(groups, d.slug)}</details>`;
    const versionPicker = `<details class="dropdown version-picker"><summary><span class="version-picker-label">Version</span><strong>${v.label}</strong>${chevron}</summary><div class="version-menu">${versions.list
      .map(
        (o) =>
          `<a href="${inVersion(o, d.slug)}"${o.id === v.id ? ' aria-current="true"' : ''}>${o.label}${o === defaultVersion ? ' <span class="muted">current release</span>' : ''}${o.released ? '' : ' <span class="muted">unreleased</span>'}</a>`,
      )
      .join('')}</div></details>`;
    const toc = d.toc.length
      ? `<nav class="toc" aria-label="On this page"><h2>On this page</h2>${d.toc.map((h) => `<a href="#${h.id}">${h.text}</a>`).join('')}</nav>`
      : '';
    const i = listed.indexOf(d);
    const prev = i > 0 ? listed[i - 1] : null;
    const next = i >= 0 ? listed[i + 1] : null;
    const pager = `<nav class="pager" aria-label="Pages">${prev ? `<a class="prev" href="${prev.slug}.html"><span>Previous</span>${prev.title}</a>` : '<span></span>'}${next ? `<a class="next" href="${next.slug}.html"><span>Next</span>${next.title}</a>` : ''}</nav>`;

    // Versioned pages say when they are not the docs most readers want; project pages are the same in every version.
    let notice = '';
    if (!isProjectPage(d) && v !== defaultVersion) {
      notice = v.released
        ? `You're reading the docs for Vyrtel ${v.label}. <a href="${inVersion(defaultVersion, d.slug)}">Read the current docs</a>.`
        : `You're reading the docs for unreleased changes on main. Some behaviour differs in the current release. <a href="${inVersion(defaultVersion, d.slug)}">Read the ${defaultVersion.label} docs</a>.`;
    }
    const source = d.ref
      ? `<a href="${repoUrl}/blob/${d.ref}/${d.file}">View the source of this page</a>`
      : `<a href="${repoUrl}/edit/main/${d.file}">Edit this page on GitHub</a>`;

    const page = fill(template, {
      HEAD: head(base, `${d.title} · Vyrtel docs`, d.description ?? `Vyrtel documentation: ${d.title}.`),
      HEADER: header(base, 'docs', d.slug),
      FOOTER: footer(base),
      VERSIONS: versionPicker,
      SIDEBAR: sidebar(groups, d.slug),
      PICKER: picker,
      GROUP: d.group,
      NOTICE: notice ? `<div class="callout version-notice" role="note"><p>${notice}</p></div>` : '',
      TOC: toc,
      CONTENT: d.html,
      PAGER: pager,
      SOURCE: source,
    });
    fs.writeFileSync(path.join(out, dir, `${d.slug}.html`), page);
  }
  // The directory itself opens the first page; retired pages forward to their replacement.
  fs.writeFileSync(path.join(out, dir, 'index.html'), redirectPage(`${listed[0].slug}.html`, 'Vyrtel docs'));
  for (const [slug, to] of Object.entries(REDIRECTS)) {
    if (!v.slugs.has(slug)) fs.writeFileSync(path.join(out, dir, `${slug}.html`), redirectPage(to, 'Moved'));
  }
  // Site-wide links (front page, header, footer) point at docs/<slug>.html. Where the default version
  // has no such page (it predates it), send them to that version's first page instead of a 404.
  if (dir === 'docs') {
    for (const d of DOCS) {
      if (!v.slugs.has(d.slug) && !(d.slug in REDIRECTS)) {
        fs.writeFileSync(path.join(out, dir, `${d.slug}.html`), redirectPage(`${listed[0].slug}.html`, 'Vyrtel docs'));
      }
    }
  }
  return listed.length;
}

/** Renders every version under docs/<id>/, and the default one at docs/ as well. */
function renderDocs() {
  const versions = docVersions();
  for (const v of versions.list) renderVersion(versions, v, `docs/${v.id}`, '../');
  const n = renderVersion(versions, versions.defaultVersion, 'docs', '');
  return { versions, n };
}

// ---------- assets ----------

function copy(from, to, filter) {
  fs.mkdirSync(path.dirname(path.join(out, to)), { recursive: true });
  fs.cpSync(path.isAbsolute(from) ? from : path.join(root, from), path.join(out, to), { recursive: true, filter });
}

function build() {
  fs.rmSync(out, { recursive: true, force: true });
  fs.mkdirSync(out, { recursive: true });

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

  const { versions, n } = renderDocs();
  const ids = versions.list.map((v) => (v === versions.defaultVersion ? `${v.id} (default)` : v.id)).join(', ');
  console.log(`Built ${path.relative(root, out)}: front page + ${n} docs pages in versions ${ids} for ${repo}, site URL ${siteUrl}`);
}

build();
