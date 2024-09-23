# Shuttle Explorer

Timeline visualiser for [Shuttle](https://github.com/awslabs/shuttle).

## Building

To use the extension, it is currently required to build it from source. Pre-requisites include:

- VS Code 1.92.0 or later
- `node` 18.20 and `npm` 10.5 (other versions are probably fine too)
- [esbuild Problem Matcher extension](https://marketplace.visualstudio.com/items?itemName=connor4312.esbuild-problem-matchers), installed either globally or in the Shuttle Explorer workspace. **Without this extension the builds will fail silently!**

Steps:

1. Clone this repository.
2. Navigate to the Shuttle Explorer directory in a terminal, run `npm install`.
3. Open the Shuttle Explorer directory in VS Code.
4. Press F5 or find "Run > Start Debugging" in the menu. Accept a debug configuration if prompted.
  - In the background, two tasks should spawn: a TypeScript watcher, and an esbuild watcher. You should be able to see both in the "terminal" window, and neither should be reporting an error. If the esbuild watcher is marked as "pending", you need the esbuild Problem Matcher extension.
  - The TypeScript sources in `src` should be compiled down to JavaScript, output into the `dist` folder.
5. A new VS Code window (the "extension host") should open.
6. (Optionally,) open a workspace in the new window, to see the code cursors.
7. From the command palette (cmd + shift + P), find "Shuttle Explorer: Focus on Home View" to reveal the extension.
8. Press the "file" button in the top-left corner of the extension panel, or use "View annotated schedule" in the command palette to pick a JSON file to visualise.

Notes:

- Things may go wrong before the extension host window is open. Look at the problems panel, see if there are any problems. Look at the terminal window, to see if the background tasks are running and not reporting any errors.
- Things may go wrong after the extension host window is open. In that window, look at the console (command palette: "Developer: Open Webview Developer Tools") to look for any errors.
- After closing the Shuttle Explorer panel, re-opening it will probably not work. Reload the window (cmd + R or command palette: "Developer: Reload Window") to start again.

During development:

- While the extension is running, changing TypeScript files in `src` should trigger an automatic re-compilation. Simply reload the window (cmd + R or command palette: "Developer: Reload Window") with the extension to see the new changes. (Sometimes, the window spontaneously closes after reloading it. Re-start with F5 if this happens.)
