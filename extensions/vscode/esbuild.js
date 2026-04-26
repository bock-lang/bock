// Bundles src/extension.ts → out/extension.js as a CommonJS module
// for the VS Code extension host. Pass --watch for incremental rebuilds,
// --production for minified output with no source maps.

const esbuild = require('esbuild');

const watch = process.argv.includes('--watch');
const production = process.argv.includes('--production');

/** @type {import('esbuild').BuildOptions} */
const options = {
  entryPoints: ['src/extension.ts'],
  bundle: true,
  outfile: 'out/extension.js',
  external: ['vscode'],
  format: 'cjs',
  platform: 'node',
  target: 'node18',
  sourcemap: !production,
  minify: production,
  logLevel: 'info',
};

async function main() {
  if (watch) {
    const ctx = await esbuild.context(options);
    await ctx.watch();
    console.log('[esbuild] watching…');
  } else {
    await esbuild.build(options);
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
