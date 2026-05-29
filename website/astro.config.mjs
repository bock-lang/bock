import { defineConfig, sessionDrivers } from 'astro/config';

import cloudflare from '@astrojs/cloudflare';

export default defineConfig({
  site: 'https://bocklang.org',
  output: 'static',
  trailingSlash: 'never',

  build: {
    format: 'directory',
  },

  // Static site: no SSR runtime, so Astro sessions are never used. The
  // @astrojs/cloudflare adapter (v13+) auto-enables a Cloudflare KV-backed
  // session store and adds a `SESSION` KV binding whenever no `session.driver`
  // is configured. That binding makes `wrangler deploy` try to auto-provision
  // the `bock-homepage-session` KV namespace, which fails once it already
  // exists. Pin a non-KV (in-memory) driver so no KV binding is emitted.
  session: {
    driver: sessionDrivers.memory(),
  },

  // Disable the runtime Cloudflare Images binding (`IMAGES`), which is
  // auto-enabled by default (imageService defaults to 'cloudflare-binding')
  // and would likewise be auto-provisioned at deploy time. 'compile' keeps
  // build-time image optimization without requiring a runtime binding.
  adapter: cloudflare({
    imageService: 'compile',
  }),
});