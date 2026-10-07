// Vyrtel website behaviour. Everything here is progressive: the page works without it.
(() => {
  document.documentElement.classList.add('js');
  const REPO = '{{REPO}}';
  const REPO_URL = '{{REPO_URL}}';
  const reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;

  // ---------- header: border once scrolled, mobile menu ----------
  const header = document.querySelector('.site-header');
  const onScroll = () => header?.classList.toggle('is-scrolled', window.scrollY > 4);
  onScroll();
  window.addEventListener('scroll', onScroll, { passive: true });

  const toggle = document.querySelector('.nav-toggle');
  const nav = document.getElementById('site-nav');
  toggle?.addEventListener('click', () => {
    const open = toggle.getAttribute('aria-expanded') !== 'true';
    toggle.setAttribute('aria-expanded', String(open));
    nav.classList.toggle('is-open', open);
  });
  nav?.addEventListener('click', (e) => {
    if (e.target.closest('a')) {
      toggle?.setAttribute('aria-expanded', 'false');
      nav.classList.remove('is-open');
    }
  });

  // ---------- copy buttons ----------
  const copyText = async (button, text) => {
    try {
      await navigator.clipboard.writeText(text);
      const label = button.textContent;
      button.textContent = 'Copied';
      button.classList.add('is-copied');
      setTimeout(() => {
        button.textContent = label;
        button.classList.remove('is-copied');
      }, 1600);
    } catch {
      // Clipboard unavailable (e.g. insecure context): leave the text selectable.
    }
  };
  document.querySelectorAll('[data-copy]').forEach((b) => b.addEventListener('click', () => copyText(b, b.dataset.copy)));
  // Docs code blocks get a copy button too.
  document.querySelectorAll('.prose pre').forEach((pre) => {
    const b = document.createElement('button');
    b.type = 'button';
    b.className = 'copy';
    b.textContent = 'Copy';
    b.setAttribute('aria-label', 'Copy code');
    b.addEventListener('click', () => copyText(b, pre.querySelector('code')?.innerText ?? pre.innerText));
    pre.appendChild(b);
  });

  // ---------- screenshot tabs ----------
  const tabs = [...document.querySelectorAll('.tour-tabs [role="tab"]')];
  const select = (tab) => {
    tabs.forEach((t) => {
      const on = t === tab;
      t.setAttribute('aria-selected', String(on));
      t.tabIndex = on ? 0 : -1;
      document.getElementById(t.getAttribute('aria-controls')).hidden = !on;
    });
  };
  tabs.forEach((t, i) => {
    t.addEventListener('click', () => select(t));
    t.addEventListener('keydown', (e) => {
      const d = e.key === 'ArrowRight' ? 1 : e.key === 'ArrowLeft' ? -1 : 0;
      if (!d) return;
      const next = tabs[(i + d + tabs.length) % tabs.length];
      select(next);
      next.focus();
    });
  });

  // ---------- downloads: highlight this OS, check a release exists ----------
  const platform = (navigator.userAgentData?.platform || navigator.platform || navigator.userAgent).toLowerCase();
  const os = platform.includes('win') ? 'windows' : platform.includes('mac') ? 'mac' : platform.includes('linux') ? 'linux' : null;
  const names = { windows: 'Windows', mac: 'macOS', linux: 'Linux' };
  const card = os && document.querySelector(`.dl[data-os="${os}"]`);
  if (card) {
    card.classList.add('is-recommended');
    document.querySelectorAll('[data-os-cta]').forEach((a) => {
      a.textContent = `Download for ${names[os]}`;
    });
  }

  if (document.querySelector('.downloads')) {
    fetch(`https://api.github.com/repos/${REPO}/releases/latest`, { headers: { Accept: 'application/vnd.github+json' } })
      .then((r) => (r.ok ? r.json() : Promise.reject(r.status)))
      .then((release) => {
        const assets = new Set((release.assets ?? []).map((a) => a.name));
        let missing = 0;
        document.querySelectorAll('.dl[data-asset]').forEach((a) => {
          if (assets.has(a.dataset.asset)) return;
          // Older releases used different file names: send people to the release page instead.
          missing += 1;
          a.href = release.html_url;
          a.querySelector('.dl-file').textContent = `See ${release.tag_name} release page`;
        });
        const note = document.querySelector('[data-release-note]');
        if (note) {
          note.innerHTML = missing
            ? `Latest release: <a href="${release.html_url}">${release.tag_name}</a>. Its files use older names, so the cards below open the release page.`
            : `Latest release: <a href="${release.html_url}">${release.tag_name}</a>. Single self-contained binaries with the web UI built in.`;
        }
      })
      .catch((status) => {
        // 404: nothing published yet. Point at the source build instead of dead download links.
        if (status !== 404) return;
        document.querySelectorAll('.dl').forEach((a) => a.classList.add('is-unavailable'));
        const note = document.querySelector('[data-release-note]');
        if (note) {
          note.innerHTML = `No release has been published yet; the binaries below appear with the first <a href="${REPO_URL}/releases">tagged release</a>. Until then, build from source or with Docker.`;
        }
      });
  }

  // ---------- reveal on scroll ----------
  const items = document.querySelectorAll('.reveal');
  if (reduceMotion || !('IntersectionObserver' in window)) {
    items.forEach((el) => el.classList.add('is-visible'));
  } else {
    const io = new IntersectionObserver(
      (entries) => {
        entries.forEach((e) => {
          if (e.isIntersecting) {
            e.target.classList.add('is-visible');
            io.unobserve(e.target);
          }
        });
      },
      { rootMargin: '0px 0px -8% 0px', threshold: 0.08 },
    );
    items.forEach((el) => io.observe(el));
  }
})();
