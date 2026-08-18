import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

export default function (pi: ExtensionAPI) {
  pi.on("agent_settled", async (_event, ctx) => {
    const rootResult = await pi.exec("git", ["rev-parse", "--show-toplevel"], {
      cwd: ctx.cwd,
    });
    const root = rootResult.stdout.trim();
    if (rootResult.code !== 0 || !root) return;

    const build = await pi.exec("cargo", ["build", "--release"], {
      cwd: root,
    });
    if (build.code !== 0) {
      ctx.ui.notify(`helm-herdr rebuild failed: ${build.stderr.trim()}`, "error");
      return;
    }

    const link = await pi.exec("herdr", ["plugin", "link", root], {
      cwd: root,
    });
    if (link.code !== 0) {
      ctx.ui.notify(`helm-herdr plugin link failed: ${link.stderr.trim()}`, "error");
      return;
    }

    ctx.ui.notify("helm-herdr rebuilt and linked", "info");
  });
}
