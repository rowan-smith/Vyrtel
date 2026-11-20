# Web UI Rules

Vyrtel is a developer tool, not a marketing dashboard. Favour information density, clarity and predictable controls.

## Visual style

- Follow `docs/brand/README.md` and reuse tokens in `web/src/styles.css`.
- Preserve the Aurora palette, Inter / IBM Plex Mono typography and existing dark/light support.
- Keep layouts compact and professional; use colour to communicate meaning, not decoration.
- Avoid new UI libraries, elaborate animation and unsolicited layout redesigns.

## React and interaction

- Reuse components and existing patterns before introducing abstractions or dependencies.
- Keep large log lists virtualised or incrementally loaded; do not retain entire result sets in the DOM.
- Provide keyboard access, visible focus, clear errors and loading/empty states.
- Avoid duplicate network requests and unnecessary rerenders on live views.

## Checks

From `web/`: run `npm run lint`, `npm run typecheck`, `npm test` and `npm run build` for relevant changes. Use `npm run format:check` to verify formatting.
