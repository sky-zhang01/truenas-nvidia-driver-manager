import { test as base } from "@playwright/test";
export * from "@playwright/test";

// The fake-host suite has no external service dependency. Block accidental browser requests
// to download links and other hosts while leaving route.fetch() on local APIs available.
export const test = base.extend({
  context: async ({ context }, use) => {
    await context.route("**/*", async (route) => {
      if (new URL(route.request().url()).hostname === "127.0.0.1") await route.continue();
      else await route.abort();
    });
    await use(context);
  },
});
