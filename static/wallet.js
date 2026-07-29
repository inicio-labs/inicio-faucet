// Connect the Bread (Miden) wallet and read the account address, to auto-fill the recipient field.
//
// We talk to the wallet's injected provider (window.midenWallet) directly. This is exactly what the
// official @miden-sdk/miden-wallet-adapter does under the hood — its connect() calls
// `window.midenWallet.connect(privateDataPermission, network)` then reads `.address`. We inline that
// here rather than bundling the npm adapter, because the adapter transitively imports the WASM
// `@miden-sdk/miden-sdk`, which is heavy and unnecessary just to connect + read an address, and
// doesn't bundle cleanly into this no-build static site.
(function () {
  var BREAD_URL = "https://chromewebstore.google.com/detail/Bread/coajhopfooegmaifelglfboehacldcbo";
  function provider() {
    return (typeof window !== "undefined" && (window.midenWallet || window.miden)) || null;
  }
  window.MidenFaucetWallet = {
    installUrl: BREAD_URL,
    isInstalled: function () {
      return !!provider();
    },
    address: function () {
      var p = provider();
      return p ? p.address || null : null;
    },
    connect: async function () {
      var p = provider();
      if (!p) throw new Error("not-installed");
      // "UPON_REQUEST" = don't request private data up front; "testnet" = network.
      await p.connect("UPON_REQUEST", "testnet");
      if (!p.address) throw new Error("no address returned from wallet");
      return p.address;
    },
    disconnect: async function () {
      var p = provider();
      if (p && typeof p.disconnect === "function") {
        try {
          await p.disconnect();
        } catch (e) {
          /* ignore */
        }
      }
    },
  };
})();
