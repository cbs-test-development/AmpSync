# AmpSync Browser Link

A small Edge/Chrome extension that hands your SharePoint sign-in to the AmpSync desktop app, so AmpSync does not ask you to sign in every time your organisation expires the session. It talks to AmpSync through native messaging on your own computer; nothing goes over the network.

## Privacy

- It reads only SharePoint cookies (`*.sharepoint.com`, `.us`, `.cn`, `.de`) and sends them only to AmpSync on this computer.
- It reads no other cookies, no history, no page content, and has no content scripts.
- It never writes cookie values to any log.
- Of the SharePoint cookies, only the host's own and the shared parent-domain ones (such as `rtFa`) are sent.

## Install (unpacked)

AmpSync's setup guide extracts this folder for you. To do it by hand:

- Edge: open `edge://extensions`, switch on Developer mode (bottom-left), choose Load unpacked, and pick this folder.
- Chrome: open `chrome://extensions`, switch on Developer mode (top-right), choose Load unpacked, and pick this folder.

The extension ID is fixed by the `key` in `manifest.json`, so it is always `foohiaphdliphaogfjlahdmbjemmaocp`. AmpSync only trusts that ID.

## Development

Pure logic is in `lib.js`; `background.js` is the service worker. Run the tests with Node 24:

```
node --test "extensions/browser-link/test/*.test.js"
```
