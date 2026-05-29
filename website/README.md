# Bock Website

Source for [bocklang.org](https://bocklang.org). Built with [Astro](https://astro.build).

## Develop

```
npm install
npm run dev      # http://localhost:4321
```

## Build

```
npm run build    # → dist/
npm run preview  # serves dist/ for verification
```

## Pages

- `src/pages/index.astro` - homepage
- `src/pages/get-started.astro` - getting started guide

Marketing copy is editorially locked - do not edit without coordination
with the marketing/spec process.

## Output

Static HTML in `dist/`. Deployed via a separate workflow (not yet
configured at the time of this commit).
