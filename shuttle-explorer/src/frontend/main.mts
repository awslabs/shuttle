import * as d3 from "d3";

declare const acquireVsCodeApi: any;

import {
    provideVSCodeDesignSystem,
    vsCodeButton,
    vsCodeCheckbox,
    vsCodeDataGrid,
    vsCodeDataGridCell,
    vsCodeDataGridRow,
    vsCodeDivider,
    vsCodeLink,
    vsCodePanels,
    vsCodePanelTab,
    vsCodePanelView,
} from "@vscode/webview-ui-toolkit";

import { Schedule } from "../common/schedule.mjs";
import {
    createContextData,
    createContextFilters,
    createContextHierarchy,
    ExtContextData,
    ExtContextFilters,
    ExtContextHierarchy,
} from "../common/context.mjs";
import { addCustomFilter, addFilter, createContextUI, ctxData, ExtContextUI } from "./ui.mts";
import { filterByEventAfter, filterByEventBefore, filterByEventConcurrent, filterByObject } from "../common/filters.mts";
import { MessageBackToFront, MessageFrontToBack } from "../common/messages.mts";

type ExtContext = {
    data: ExtContextData,
    hierarchy: ExtContextHierarchy,
    filters: ExtContextFilters,
    ui: ExtContextUI,
};

export let vscode: any = null;

export function sendMessage(message: MessageFrontToBack) {
    vscode.postMessage(message);
}

document.addEventListener("DOMContentLoaded", () => {
    vscode = acquireVsCodeApi();

    provideVSCodeDesignSystem().register(
        vsCodeButton(),
        vsCodeCheckbox(),
        vsCodeDataGrid(),
        vsCodeDataGridCell(),
        vsCodeDataGridRow(),
        vsCodeDivider(),
        vsCodeLink(),
        vsCodePanels(),
        vsCodePanelTab(),
        vsCodePanelView(),
    );

    function createTimeline(schedule: Schedule) {
        let ctxPartial: {
            data?: ExtContextData,
            hierarchy?: ExtContextHierarchy,
            filters?: ExtContextFilters,
            ui?: ExtContextUI,
        } = {};
        ctxPartial.data = createContextData(schedule);
        console.log("schedule stats", {
            taskCount: ctxPartial.data.tasks.length,
            eventCount: ctxPartial.data.events.length,
            objectCount: ctxPartial.data.objects.length,
        });
        ctxPartial.hierarchy = createContextHierarchy(ctxPartial.data);
        ctxPartial.filters = createContextFilters(ctxPartial.data);
        ctxPartial.ui = createContextUI(ctxPartial.data, ctxPartial.filters, ctxPartial.hierarchy);
        let _ctx: ExtContext = ctxPartial as ExtContext;
    }

    // reload schedule
    d3.select("#reload-schedule")
        .on("click", () => sendMessage({ type: "reloadSchedule" }));

    window.addEventListener("message", (event: MessageEvent<MessageBackToFront>) => {
        const {type, data} = event.data;
        switch (type) {
            case "createEventFilter":
                switch (data.kind) {
                    case "before":
                        addFilter(filterByEventBefore(ctxData!, data.eventId), false);
                        break;
                    case "after":
                        addFilter(filterByEventAfter(ctxData!, data.eventId), false);
                        break;
                    case "concurrent":
                        addFilter(filterByEventConcurrent(ctxData!, data.eventId), false);
                        break;
                }
                break;
            case "createObjectFilter":
                addFilter(filterByObject(ctxData!, data.objectId, data.keepObject), false);
                break;
            case "createTimeline":
                createTimeline(data);
                break;
            case "updateCustomFilter":
                addCustomFilter(data.name, data.source);
                break;
        }
    });
});
