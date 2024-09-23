const esbuild = require("esbuild");

const production = process.argv.includes("--production");
const watch = process.argv.includes("--watch");

/**
 * @type {import("esbuild").Plugin}
 */
// note: the [watch] prefix here is important, see
// https://github.com/connor4312/esbuild-problem-matchers/blob/51e17a9f4464dd008bfc07871482b94dc87901ce/package.json#L97
const esbuildProblemMatcherPlugin = {
  name: "esbuild-problem-matcher",

  setup(build) {
    build.onStart(() => {
      console.log("[watch] build started");
    });
    build.onEnd((result) => {
      result.errors.forEach(({ text, location }) => {
        console.error(`✘ [ERROR] ${text}`);
        console.error(`    ${location.file}:${location.line}:${location.column}:`);
      });
      console.log("[watch] build finished");
    });
  },
};

async function mainBackend() {
  const ctx = await esbuild.context({
    entryPoints: [
      "src/backend/main.mts"
    ],
    bundle: true,
    format: "cjs",
    minify: production,
    sourcemap: !production,
    sourcesContent: false,
    platform: "node",
    outfile: "dist/backend/main.js",
    external: ["vscode"],
    logLevel: "silent",
    plugins: [
      esbuildProblemMatcherPlugin,
    ],
  });
  if (watch) {
    await ctx.watch();
  } else {
    await ctx.rebuild();
    await ctx.dispose();
  }
}

async function mainFrontend() {
  const ctx = await esbuild.context({
    entryPoints: [
      "src/frontend/main.mts" 
    ],
    bundle: true,
    format: "esm",
    minify: production,
    sourcemap: !production,
    sourcesContent: false,
    platform: "browser",
    outfile: "dist/frontend/main.js",
    external: [],
    logLevel: "silent",
    plugins: [
      esbuildProblemMatcherPlugin,
    ],
  });
  if (watch) {
    await ctx.watch();
  } else {
    await ctx.rebuild();
    await ctx.dispose();
  }
}

Promise.any([mainBackend(), mainFrontend()]).catch(e => {
  console.error(e);
  process.exit(1);
});
