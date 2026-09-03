import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

export default function (pi: ExtensionAPI) {
  pi.on("agent_settled", async (_event, ctx) => {
    const rootResult = await pi.exec("git", ["rev-parse", "--show-toplevel"], {
      cwd: ctx.cwd,
    });
    const root = rootResult.stdout.trim();
    if (rootResult.code !== 0 || !root) return;

    const install = await pi.exec("./scripts/install-local.sh", [], {
      cwd: root,
    });
    if (install.code !== 0) {
      ctx.ui.notify(`helm-herdr install failed: ${install.stderr.trim()}`, "error");
      return;
    }

    ctx.ui.notify("helm-herdr built, installed, and linked", "info");
  });
}
