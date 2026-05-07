import { defineConfig } from 'astro/config';

export default defineConfig({
  site: 'https://bocklang.org',
  output: 'static',
  trailingSlash: 'never',
  build: {
    format: 'directory',
  },
});
