import { defineConfig } from 'astro/config';

import cloudflare from "@astrojs/cloudflare";

export default defineConfig({
  site: 'https://bocklang.org',
  output: "hybrid",
  trailingSlash: 'never',

  build: {
    format: 'directory',
  },

  adapter: cloudflare()
});