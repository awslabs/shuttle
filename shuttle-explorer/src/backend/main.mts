import * as vscode from "vscode";
import * as jsonlint from "jsonlint-pos";

// to decorate the JSON itself
const decorationSchedule = vscode.window.createTextEditorDecorationType({
    backgroundColor: "#4d0",
});

// to place cursors in the code corresponding to tasks
const decorationCode = vscode.window.createTextEditorDecorationType({
    after: {
        backgroundColor: "#4d0",
        color: "#090",
        height: "1.1em",
        margin: "1px",
    },
});

import { Schedule, ScheduleEvent, ProcessedSchedule, ScheduleV0 } from "../common/schedule.mjs";
import { MessageBackToFront, MessageFrontToBack } from "../common/messages.mts";

export function activate(context: vscode.ExtensionContext) {
    // TODO: the panel does not show again after closing, the entire window has to be reloaded
    console.log("Shuttle Explorer activated");
    const explorer = new Explorer(context.extensionUri);
    vscode.window.onDidChangeActiveTextEditor((editor) => explorer.decorateEditor(editor));
    context.subscriptions.push(vscode.window.registerWebviewViewProvider(Explorer.viewType, explorer));
    context.subscriptions.push(vscode.commands.registerCommand("shuttle-explorer.view", () => explorer.askForSchedule()));
}

export function deactivate() {}

type CustomFilter = {
    path: vscode.Uri,
    watcher: vscode.FileSystemWatcher,
};

class Explorer implements vscode.WebviewViewProvider {
    // matches the view ID in package.json
    public static readonly viewType = "shuttleExplorerPanel.views.home";
    private _view?: vscode.WebviewView;

    // schedule
    private _schedulePath: vscode.Uri | null = null;
    private _scheduleString: string | null = null;
    private _annotatedSchedule?: ScheduleV0;
    private _annotatedScheduleWithPos: any | null = null;

    // code cursors
    private _activeEditor?: vscode.TextEditor;
    private _previewEvent?: [number, ScheduleEvent] | null;
    private _selectedEvent?: [number, ScheduleEvent];
    private _processedSchedule?: ProcessedSchedule;

    // custom filters
    private _customFilters: CustomFilter[] = [];

    constructor(
        private readonly _extensionUri: vscode.Uri,
    ) {}

    private _processSchedule(
        schedule: ScheduleV0,
    ) {
        // process the schedule so that we know, for each event, which other
        // tasks are concurrently executing and thus which code cursors should
        // be shown
        let tasksRunning: number[] = [];
        let lastEventByTask: Map<number, number> = new Map();
        let eventId = 0;
        let previousEvents: ScheduleEvent[][] = [];
        for (const [taskId, _info, kind] of schedule.events) {
            if (typeof kind === "string" || kind instanceof String) {
                if (kind === "TaskTerminated") {
                    tasksRunning.splice(tasksRunning.indexOf(taskId), 1);
                }
            } else {
                if ("TaskCreated" in kind) {
                    tasksRunning.push(kind["TaskCreated"][0]);
                }
            }
            lastEventByTask.set(taskId, eventId);
            const previous = tasksRunning.sort()
                .filter(otherId => taskId !== otherId)
                .filter(otherId => lastEventByTask.has(otherId))
                .map(otherId => schedule.events[lastEventByTask.get(otherId)!]);
            previousEvents.push(previous);
            eventId++;
        }
        this._processedSchedule = {
            previousEvents,
        };
    }

    public async askForSchedule() {
        // ask for schedule
        const schedule = await vscode.window.showOpenDialog({
            canSelectFiles: true,
            canSelectFolders: false,
            canSelectMany: false,
            filters: {
                "Schedule": ["json"],
            },
        });
        if (schedule) {
            this.setSchedulePath(schedule[0]);
        }
    }

    public setSchedulePath(
        path: vscode.Uri,
    ) {
        this._schedulePath = path;
        this._loadSchedule();
    }

    private _loadSchedule() {
        if (this._schedulePath === null) { return; }
        vscode.workspace.fs.readFile(this._schedulePath).then((scheduleRaw: Uint8Array) => {
            this._scheduleString = new TextDecoder("utf-8").decode(scheduleRaw);
            const schedule: Schedule = JSON.parse(this._scheduleString);
            switch (schedule.version) {
                case 0:
                    this.setAnnotatedSchedule(schedule as ScheduleV0);
                    this.decorateEditor(vscode.window.activeTextEditor);
                    break;
                default:
                    vscode.window.showErrorMessage(`Schedule version ${schedule.version} not supported.`);
            }
        }, (reason) => {
            console.error(reason);
            vscode.window.showErrorMessage("Could not load schedule.");
        });
    }

    public setAnnotatedSchedule(
        schedule: ScheduleV0,
    ) {
        this._annotatedSchedule = schedule;
        this._annotatedScheduleWithPos = null;
        this._previewEvent = null;
        this._selectedEvent = [0, schedule.events[0]];
        this._sendMessage({
            type: "createTimeline",
            data: schedule,
        });
        this._processSchedule(schedule);
    }

    public decorateEditor(
        editor?: vscode.TextEditor,
    ) {
        this._activeEditor = editor;
        this._updateDecorations();
    }

    private _sendMessage(message: MessageBackToFront) {
        if (this._view) {
            this._view.webview.postMessage(message);
        }
    }

    public resolveWebviewView(
        webviewView: vscode.WebviewView,
        _context: vscode.WebviewViewResolveContext,
        _token: vscode.CancellationToken,
    ) {
        this._view = webviewView;
        webviewView.webview.options = {
            // allow scripts in the webview
            enableScripts: true,
            localResourceRoots: [
                this._extensionUri,
            ],
        };
        webviewView.webview.html = this._getHtmlForWebview(webviewView.webview);
        webviewView.webview.onDidReceiveMessage((msg: MessageFrontToBack) => { switch (msg.type) {
            case "backtraceClick": this._onBacktraceClick(msg.data[0], msg.data[1]); break;
            case "createCustomFilter":
                vscode.window.showOpenDialog({
                    canSelectFiles: true,
                    canSelectFolders: false,
                    canSelectMany: false,
                    filters: {
                        "Filter": ["js"],
                    },
                }).then((path) => {
                    if (!path || path.length !== 1) { return; }
                    const filterPath = path[0];
                    const watcher = vscode.workspace.createFileSystemWatcher(
                        filterPath.fsPath, // TODO: is this right? it's not really a glob pattern ...
                    );
                    const filter: CustomFilter = {
                        path: filterPath,
                        watcher,
                    };
                    const cleanup = () => {
                        this._customFilters.splice(this._customFilters.indexOf(filter), 1);
                        watcher.dispose();
                    }
                    const update = () => vscode.workspace.fs.readFile(filterPath).then((sourceRaw: Uint8Array) => {
                        const source = new TextDecoder("utf-8").decode(sourceRaw);
                        this._sendMessage({
                            type: "updateCustomFilter",
                            data: {
                                name: filterPath.fsPath,
                                source,
                            },
                        });
                    }, (reason) => {
                        console.error(reason);
                        vscode.window.showErrorMessage("Could not load custom filter.");
                        cleanup();
                    });
                    watcher.onDidChange(() => update());
                    watcher.onDidDelete(() => cleanup());
                    this._customFilters.push(filter);
                    update();
                });
                break;
            case "createEventFilterStart":
                // "Create filter" pressed in event panel, ask user what they want
                vscode.window.showQuickPick([
                    {
                        filterKind: "before",
                        label: "causally before",
                        description: "Filter will show only events that causally precede (happen-before) the selected event.",
                    }, {
                        filterKind: "after",
                        label: "causally after",
                        description: "Filter will show only events that causally succeed (happen-after) the selected event.",
                    }, {
                        filterKind: "concurrent",
                        label: "concurrent",
                        description: "Filter will show only events that are concurrent with the selected event, i.e., are not causally related.",
                    },
                ], {
                    title: "Which type of filter should be created?",
                }).then((chosen) => {
                    if (!chosen) return;
                    this._sendMessage({
                        type: "createEventFilter",
                        data: {
                            eventId: msg.data,
                            kind: chosen.filterKind as ("before" | "after" | "concurrent"),
                        },
                    });
                });
                break;
            case "createObjectFilterStart":
                // "Create filter" pressed in object panel, ask user what they want
                vscode.window.showQuickPick([
                    {
                        keepObject: true,
                        label: "with object",
                        description: "Filter will show only events that relate to the object.",
                    }, {
                        keepObject: false,
                        label: "without object",
                        description: "Filter will hide events that relate to the object.",
                    },
                ], {
                    title: "Which type of filter should be created?",
                }).then((chosen) => {
                    if (!chosen) return;
                    this._sendMessage({
                        type: "createObjectFilter",
                        data: {
                            objectId: msg.data,
                            keepObject: chosen.keepObject,
                        },
                    });
                });
                break;
            case "eventHover": this._onEventHover(msg.data); break;
            case "eventHoverOff": this._onEventHoverOff(msg.data); break;
            case "eventClick":
                this._ensureFocus();
                this._onEventClick(msg.data);
                break;
            case "objectClick": this._ensureFocus(); break;
            case "reloadSchedule":
                if (this._schedulePath) {
                    this._loadSchedule();
                } else {
                    this.askForSchedule();
                }
                break;
            case "taskClick": this._ensureFocus(); break;
        } });
    }

    private _ensureFocus() {
        if (!this._view) { return; }
        this._view.show();
    }

    private _onEventHover(
        event_id: number,
    ) {
        if (!this._annotatedSchedule
            || !this._annotatedSchedule.events[event_id]
            || !this._selectedEvent) { return; }
        if (this._selectedEvent[0] === event_id) {
            this._previewEvent = null;
            return;
        }
        this._previewEvent = [event_id, this._annotatedSchedule.events[event_id]];
        this._updateDecorations();
    }

    private _onEventHoverOff(
        eventId: number,
    ) {
        if (!this._annotatedSchedule
            || !this._annotatedSchedule.events[eventId]
            || !this._previewEvent) { return; }
        if (this._previewEvent[0] === eventId) {
            this._previewEvent = null;
            this._updateDecorations();
        }
    }

    private _onEventClick(
        eventId: number,
    ) {
        if (!this._annotatedSchedule
            || !this._annotatedSchedule.events[eventId]) { return; }
        this._selectedEvent = [eventId, this._annotatedSchedule.events[eventId]];
        if (this._previewEvent && this._selectedEvent[0] === this._previewEvent[0]) {
            this._previewEvent = null;
        }
        this._updateDecorations();
    }

    private _onBacktraceClick(
        eventId: number,
        btId: number,
    ) {
        if (!this._activeEditor
            || !this._annotatedSchedule
            || !this._annotatedSchedule.events[eventId]
            || !this._annotatedSchedule.events[eventId][1]) { return; }
        const [pathId, _functionId, line, col] = this._annotatedSchedule.events[eventId][1][btId];
        const path = vscode.Uri.joinPath(
            vscode.workspace.workspaceFolders![0].uri,
            this._annotatedSchedule.files[pathId].path,
        );
        vscode.window.showTextDocument(path, {
            preview: true,
            selection: new vscode.Selection(
                line - 1, col - 1,
                line - 1, col - 1,
            ),
        });
    }

    private _updateDecorations() {
        if (!this._activeEditor
            || !this._annotatedSchedule) { return; }
        const activePath = this._activeEditor.document.uri;

        const decorations: vscode.DecorationOptions[] = [];
        const eventId = this._selectedEvent![0];

        // if we are looking at the schedule JSON, highlight the current event
        if (activePath.toString() === this._schedulePath?.toString()) {
            // this is ugly, but it turns out parsing JSON while preserving
            // the source positions is not such a common task, so we use
            // a niche library
            if (this._annotatedScheduleWithPos === null) {
                jsonlint.parser.setPosEnabled(true);
                this._annotatedScheduleWithPos = (jsonlint as any).parse(this._scheduleString);
            }

            const {
                first_line,
                last_line,
                first_column,
                last_column,
            } = this._annotatedScheduleWithPos.events._pos[`_${eventId}`];
            decorations.push({
                range: new vscode.Range(first_line - 1, first_column, last_line - 1, last_column),
            });

            this._activeEditor.setDecorations(decorationSchedule, decorations);
            this._activeEditor.setDecorations(decorationCode, []);
            return;
        }

        const eventsToShow = [this._selectedEvent![1]].concat(this._processedSchedule!.previousEvents[eventId]);

        for (const [taskId, info, kind] of eventsToShow) {
            if (info === null) { continue; }
            const [pathIdx, _functionIdx, line, col] = info[0];

            const eventPath = vscode.Uri.joinPath(
                vscode.workspace.workspaceFolders![0].uri,
                this._annotatedSchedule!.files[pathIdx].path,
            );
            if (activePath.toString() !== eventPath.toString()) {
                continue;
            }

            decorations.push({
                range: new vscode.Range(line - 1, col - 1, line - 1, col - 1),
                renderOptions: {
                    after: {
                        contentText: "#" + taskId,
                    },
                },
            });
        }
        // if (this._previewEvent) addEvent(this._previewEvent[1]);

        this._activeEditor.setDecorations(decorationSchedule, []);
        this._activeEditor.setDecorations(decorationCode, decorations);
    }

    private _getHtmlForWebview(webview: vscode.Webview) {
        // prepare URIs for local stylesheets
        const styleResetUri = webview.asWebviewUri(vscode.Uri.joinPath(this._extensionUri, "media", "reset.css"));
        const styleVSCodeUri = webview.asWebviewUri(vscode.Uri.joinPath(this._extensionUri, "media", "vscode.css"));
        const styleMainUri = webview.asWebviewUri(vscode.Uri.joinPath(this._extensionUri, "media", "main.css"));

        // package-sourced
        const styleCodiconsUri = webview.asWebviewUri(vscode.Uri.joinPath(this._extensionUri, "node_modules", "@vscode/codicons", "dist", "codicon.css"));
        const toolkitUri = webview.asWebviewUri(vscode.Uri.joinPath(this._extensionUri, "node_modules", "@vscode/webview-ui-toolkit", "dist", "toolkit.min.js"));
        const d3Uri = webview.asWebviewUri(vscode.Uri.joinPath(this._extensionUri, "node_modules", "d3", "dist", "d3.min.js"));

        // frontend Javascript
        const scriptUri = webview.asWebviewUri(vscode.Uri.joinPath(this._extensionUri, "dist", "frontend", "main.js"));

        const nonce = getNonce();
        // TODO: move to separate file
        // TODO: many of the "classes" here should actually be "id"s
        return /*html*/`<!DOCTYPE html>
            <html lang="en">
            <head>
                <meta charset="UTF-8">
                <!-- unsafe-eval is a bit sketchy, it's only here for custom filters... -->
                <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src ${webview.cspSource} 'unsafe-inline'; font-src ${webview.cspSource}; script-src 'nonce-${nonce}' 'unsafe-eval'">
                <meta name="viewport" content="width=device-width, initial-scale=1.0">
                <link href="${styleResetUri}" rel="stylesheet">
                <link href="${styleVSCodeUri}" rel="stylesheet">
                <link href="${styleCodiconsUri}" rel="stylesheet">
                <link href="${styleMainUri}" rel="stylesheet">
                <title>Shuttle Explorer</title>
                <script nonce="${nonce}" type="text/javascript" src="${d3Uri}"></script>
                <script nonce="${nonce}" type="module" src="${toolkitUri}"></script>
                <script nonce="${nonce}" type="module" src="${scriptUri}"></script>
            </head>
            <body>
                <main>
                    <div class="panel-topleft">
                        <a id="reload-schedule" title="Load or reload schedule file">
                            <i class="codicon codicon-file"></i>
                        </a><a id="toggle-cdag" title="Show causal graph">
                            <i class="codicon codicon-repo-forked"></i>
                        </a><a id="toggle-objects" class="selected" title="Show objects">
                            <i class="codicon codicon-symbol-field"></i>
                        </a><a id="reveal-selected" title="Scroll to selected task/event/object">
                            <i class="codicon codicon-eye"></i>
                        </a>
                    </div>
                    <svg class="panel-top"></svg>
                    <div class="panel-sidebar">
                        <vscode-panels activeid="sidebar-tab-1">
                            <vscode-panel-tab id="sidebar-tab-1">SELECTION</vscode-panel-tab>
                            <vscode-panel-tab id="sidebar-tab-2">FILTERS</vscode-panel-tab>
                            <vscode-panel-view id="sidebar-view-1">
                                <p class="placeholder">Select an event, task, or object.</p>
                                <div class="event-info">
                                    <vscode-data-grid grid-template-columns="2fr 3fr">
                                        <vscode-data-grid-row>
                                            <vscode-data-grid-cell grid-column="1">Kind</vscode-data-grid-cell>
                                            <vscode-data-grid-cell grid-column="2" class="event-info-kind">-</vscode-data-grid-cell>
                                        </vscode-data-grid-row>
                                        <vscode-data-grid-row>
                                            <vscode-data-grid-cell grid-column="1">Time</vscode-data-grid-cell>
                                            <vscode-data-grid-cell grid-column="2" class="event-info-time">-</vscode-data-grid-cell>
                                        </vscode-data-grid-row>
                                        <vscode-data-grid-row>
                                            <vscode-data-grid-cell grid-column="1">Task</vscode-data-grid-cell>
                                            <vscode-data-grid-cell grid-column="2" class="event-info-task">-</vscode-data-grid-cell>
                                        </vscode-data-grid-row>
                                        <vscode-data-grid-row>
                                            <vscode-data-grid-cell grid-column="1">Extra data</vscode-data-grid-cell>
                                            <vscode-data-grid-cell grid-column="2" class="event-info-extra">-</vscode-data-grid-cell>
                                        </vscode-data-grid-row>
                                    </vscode-data-grid>
                                    <div class="event-info-backtrace-holder">
                                        <vscode-divider></vscode-divider>
                                        <vscode-data-grid class="event-info-backtrace" grid-template-columns="2fr 3fr">
                                            <vscode-data-grid-row row-type="header">
                                                <vscode-data-grid-cell grid-column="1" cell-type="columnheader">Function</vscode-data-grid-cell>
                                                <vscode-data-grid-cell grid-column="2" cell-type="columnheader">Path</vscode-data-grid-cell>
                                            </vscode-data-grid-row>
                                        </vscode-data-grid>
                                    </div>
                                    <vscode-button class="event-create-filter">Create filter</vscode-button>
                                </div>
                                <div class="task-info">
                                    <vscode-data-grid grid-template-columns="2fr 3fr">
                                        <vscode-data-grid-row>
                                            <vscode-data-grid-cell grid-column="1">Kind</vscode-data-grid-cell>
                                            <vscode-data-grid-cell grid-column="2" class="task-info-kind">-</vscode-data-grid-cell>
                                        </vscode-data-grid-row>
                                        <vscode-data-grid-row>
                                            <vscode-data-grid-cell grid-column="1">Name</vscode-data-grid-cell>
                                            <vscode-data-grid-cell grid-column="2" class="task-info-name">-</vscode-data-grid-cell>
                                        </vscode-data-grid-row>
                                        <vscode-data-grid-row>
                                            <vscode-data-grid-cell grid-column="1">ID</vscode-data-grid-cell>
                                            <vscode-data-grid-cell grid-column="2" class="task-info-id">-</vscode-data-grid-cell>
                                        </vscode-data-grid-row>
                                        <vscode-data-grid-row>
                                            <vscode-data-grid-cell grid-column="1">Created by</vscode-data-grid-cell>
                                            <vscode-data-grid-cell grid-column="2" class="task-info-created">-</vscode-data-grid-cell>
                                        </vscode-data-grid-row>
                                        <vscode-data-grid-row>
                                            <vscode-data-grid-cell grid-column="1">First step</vscode-data-grid-cell>
                                            <vscode-data-grid-cell grid-column="2" class="task-info-first">-</vscode-data-grid-cell>
                                        </vscode-data-grid-row>
                                        <vscode-data-grid-row>
                                            <vscode-data-grid-cell grid-column="1">Last step</vscode-data-grid-cell>
                                            <vscode-data-grid-cell grid-column="2" class="task-info-last">-</vscode-data-grid-cell>
                                        </vscode-data-grid-row>
                                    </vscode-data-grid>
                                </div>
                                <div class="object-info">
                                    <vscode-data-grid grid-template-columns="2fr 3fr">
                                        <vscode-data-grid-row>
                                            <vscode-data-grid-cell grid-column="1">Kind</vscode-data-grid-cell>
                                            <vscode-data-grid-cell grid-column="2" class="object-info-kind">-</vscode-data-grid-cell>
                                        </vscode-data-grid-row>
                                        <vscode-data-grid-row>
                                            <vscode-data-grid-cell grid-column="1">Name</vscode-data-grid-cell>
                                            <vscode-data-grid-cell grid-column="2" class="object-info-name">-</vscode-data-grid-cell>
                                        </vscode-data-grid-row>
                                        <vscode-data-grid-row>
                                            <vscode-data-grid-cell grid-column="1">ID</vscode-data-grid-cell>
                                            <vscode-data-grid-cell grid-column="2" class="object-info-id">-</vscode-data-grid-cell>
                                        </vscode-data-grid-row>
                                        <vscode-data-grid-row>
                                            <vscode-data-grid-cell grid-column="1">Created by</vscode-data-grid-cell>
                                            <vscode-data-grid-cell grid-column="2" class="object-info-created">-</vscode-data-grid-cell>
                                        </vscode-data-grid-row>
                                        <vscode-data-grid-row>
                                            <vscode-data-grid-cell grid-column="1">Seen by</vscode-data-grid-cell>
                                            <vscode-data-grid-cell grid-column="2" class="object-info-seen">-</vscode-data-grid-cell>
                                        </vscode-data-grid-row>
                                        <vscode-data-grid-row>
                                            <vscode-data-grid-cell grid-column="1">First step</vscode-data-grid-cell>
                                            <vscode-data-grid-cell grid-column="2" class="object-info-first">-</vscode-data-grid-cell>
                                        </vscode-data-grid-row>
                                        <vscode-data-grid-row>
                                            <vscode-data-grid-cell grid-column="1">Last step</vscode-data-grid-cell>
                                            <vscode-data-grid-cell grid-column="2" class="object-info-last">-</vscode-data-grid-cell>
                                        </vscode-data-grid-row>
                                    </vscode-data-grid>
                                    <vscode-button class="object-create-filter">Create filter</vscode-button>
                                </div>
                            </vscode-panel-view>
                            <vscode-panel-view id="sidebar-view-2">
                                <vscode-checkbox id="filters-tick">Tick events <vscode-badge>0</vscode-badge></vscode-checkbox>
                                <vscode-checkbox id="filters-semaphore" checked>Semaphore events <vscode-badge>0</vscode-badge></vscode-checkbox>
                                <vscode-checkbox id="filters-task" checked>Task events <vscode-badge>0</vscode-badge></vscode-checkbox>
                                <vscode-checkbox id="filters-random" checked>Random events <vscode-badge>0</vscode-badge></vscode-checkbox>
                                <vscode-button id="filters-add">Add custom filter</vscode-button>
                                <vscode-divider></vscode-divider>
                                <div id="filters-user"></div>
                            </vscode-panel-view>
                        </vscode-panels>
                    </div>
                    <div class="panel-main"></div>
                </main>
            </body>
            </html>`;
    }
}

function getNonce() {
    let text = "";
    const possible = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    for (let i = 0; i < 32; i++) {
        text += possible.charAt(Math.floor(Math.random() * possible.length));
    }
    return text;
}
