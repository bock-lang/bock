import { defineConfig } from 'astro/config';

import cloudflare from '@astrojs/cloudflare';

export default defineConfig({
  site: 'https://bocklang.org',
  output: 'static',
  trailingSlash: 'never',

  build: {
    format: 'directory',
  },

  adapter: cloudflare(),
});