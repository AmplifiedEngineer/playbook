# How to install Playbook without Homebrew

<!-- verify: names -->

Install the binary with the install script, pin a version if you want one, then run setup.

**You need:** `curl` and `bash`. The script checks the download's SHA256 checksum.

1. Run the install script.

   ```bash
   curl -fsSL https://raw.githubusercontent.com/amplifiedengineer/playbook/main/install.sh | bash
   ```

2. To pin a version, set `PLAYBOOK_REF` to a release tag from the releases page, in place of `<tag>`.

   ```bash
   curl -fsSL https://raw.githubusercontent.com/amplifiedengineer/playbook/main/install.sh | PLAYBOOK_REF=<tag> bash
   ```

3. Run setup.

   ```bash
   playbook setup
   ```

4. To add the Claude Code plugin without setup, run this with `claude` on your `PATH`.

   ```bash
   playbook plugin install
   ```

## What you see

The script downloads the release binary, checks it and puts it on your `PATH`. It asks whether to run `playbook setup`, so step 3 is for when you said no.

## Next

- [Install guide](../guides/00-install.md): Nix and Windows, plus a route with no `curl | bash`.
- [What setup installs](../guides/05-what-setup-installs.md): the changes before you agree.
