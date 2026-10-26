Sample outputs used by unit tests.

- apt-mark-showmanual.txt: captured on the development machine (Ubuntu, WSL2).
- dnf*/brew*: written by hand from the documented formats; no dnf or brew was
  available while writing them. The optional container tests check the real
  tools in CI.
- npm-ls-parseable.txt / npm-ls-enoent.stderr.txt: real npm 11.12.1 (node 24.15)
  output, generated in a throwaway prefix so the machine's own npm is untouched.
  The packages are local stub tarballs (just a package.json) with the real names,
  plus `npm` and `corepack`, which node ships in every global prefix:

      T=$(mktemp -d); cd "$T"; touch npmrc gnpmrc
      export NPM_CONFIG_PREFIX="$T/prefix" NPM_CONFIG_CACHE="$T/cache" \
             NPM_CONFIG_USERCONFIG="$T/npmrc" NPM_CONFIG_GLOBALCONFIG="$T/gnpmrc"
      mk() { mkdir -p "src/$2"; printf '{"name":"%s","version":"%s"}\n' "$1" "$3" > "src/$2/package.json";
             (cd "src/$2" && npm pack --silent --pack-destination "$T/tgz"); }
      mkdir tgz
      mk @anthropic-ai/claude-code claude-code 2.0.0; mk @openai/codex codex 0.50.0
      mk pyright pyright 1.1.400; mk npm npm 11.0.0; mk corepack corepack 0.34.0
      npm install -g ./tgz/*.tgz
      npm ls -g --depth=0 --parseable > npm-ls-parseable.txt
      # A prefix without lib/ (nothing installed yet): exit 254, ENOENT on stderr.
      mkdir empty; NPM_CONFIG_PREFIX="$T/empty" npm ls -g --depth=0 --parseable 2> npm-ls-enoent.stderr.txt

  Note the first line of the parseable output is `<prefix>/lib`, not the prefix.
