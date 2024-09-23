/**
 * Functions and types related to selections, i.e., clicking on a task, event,
 * or object in the timeline and the right panel displaying additional info.
 */

import * as d3 from "d3";

import { EventId } from "../common/events.mjs";
import { Object, ObjectId } from "../common/objects.mjs";
import { Task, TaskId } from "../common/tasks.mjs";
import { ctx, ctxData } from "./ui.mts";
import { updateTimelineLinks } from "./timeline.mts";
import { clockCmp, Ordering } from "../common/clocks.mts";
import { sendMessage } from "./main.mts";

export type ExtContextUISelection = {
    eventId: EventId | null,
    taskId: TaskId | null,
    objectId: ObjectId | null,
};
export function createContextUISelection(): ExtContextUISelection {
    return {
        eventId: null,
        taskId: null,
        objectId: null,
    };
}

function selectEventImpl(eventId: EventId | null, doSendMessage: boolean, openTab: boolean) {
    ctx!.timeline.events!
        .classed("selected", false)
        .classed("before", false)
        .classed("after", false);
    ctx!.selection.eventId = eventId;
    if (eventId !== null) {
        if (doSendMessage) {
            sendMessage({ type: "eventClick", data: eventId });
        }

        const event = ctx!.events[eventId];
        const taskId = event.event.taskId;

        ctx!.timeline.events!
            .classed("before", ({maxData}) =>
                maxData.event.id < event.event.id
                && (maxData.event.taskId === taskId || clockCmp(maxData.event, event.event) === Ordering.Less))
            .classed("after", ({minData}) =>
                event.event.id < minData.event.id
                && (minData.event.taskId === taskId || clockCmp(event.event, minData.event) === Ordering.Less));

        ctx!.timeline.events!
            .filter(({minData, maxData}) => minData.event.taskId === taskId && minData.event.id <= eventId && eventId <= maxData.event.id)
            //.filter(e => e.event.taskId === taskId && e.event.id <= eventId && eventId <= e.event.id)
            .classed("selected", true);

        // select tab
        if (openTab) {
            (d3.select(".panel-sidebar vscode-panels").node()! as any).activeid = "sidebar-tab-1";
        }

        // fill in sidebar
        const infoRoot = d3.select("#sidebar-view-1")
            .classed("show-event", true)
            .select(".event-info");
        const infoKind = event.event.kind; // TODO: human-readable description?
        infoRoot
            .select(".event-info-kind")
            .text(infoKind);
        infoRoot
            .select(".event-info-time")
            .html(`<vscode-link>Step ${event.event.id}</vscode-link> (${event.filtered ? "filtered" : `${event.keptIdx} after filters`})`)
            .select("vscode-link")
            .on("click", () => selectEvent(event.event.id, true));
        infoRoot
            .select(".event-info-task")
            .html(`<vscode-link>Task ${event.event.taskId}</vscode-link>`)
            .select("vscode-link")
            .on("click", () => selectTask(event.event.taskId, true));
        infoRoot
            .select(".event-info-extra")
            .text(event.event.extra ? JSON.stringify(event.event.extra) : "-");

        const backtrace = event.event.backtrace;
        infoRoot
            .select(".event-info-backtrace-holder")
            .classed("show", !!backtrace);
        if (backtrace !== null) {
            let rows = infoRoot.select(".event-info-backtrace")
                .selectAll("vscode-data-grid-row[row-type=\"default\"]")
                .data(event.event.backtrace ? event.event.backtrace : [])
                .join("vscode-data-grid-row")
                    .attr("row-type", "default");
            rows.selectAll("vscode-data-grid-cell.col1")
                .data(d => [d])
                .join("vscode-data-grid-cell")
                    .classed("col1", true)
                    .attr("grid-column", "1")
                    .html(({functionName}) => `<code>${functionName}</code>`);
            rows.selectAll("vscode-data-grid-cell.col2")
                .data(d => [d])
                .join("vscode-data-grid-cell")
                    .classed("col2", true)
                    .attr("grid-column", "2")
                    .html(({path, line, col}) => `<code>${path}:${line}:${col}</code>`)
                    .on("click", (_ev, {btId}) => sendMessage({ type: "backtraceClick", data: [eventId, btId] }));
        }

        // filter setup
        infoRoot.select(".event-create-filter")
            .on("click", () => sendMessage({ type: "createEventFilterStart", data: eventId }));

        // show links
        switch (event.event.kind) {
            case "TaskCreated":
                if (event.event.data.taskId !== 0) {
                    ctx!.links.current.push({
                        source: event,
                        target: ctx!.tasks[event.event.data.taskId],
                    });
                }
                break;
            case "SemaphoreCreated":
            case "SemaphoreClosed":
            case "SemaphoreAcquireFast":
            case "SemaphoreAcquireBlocked":
            case "SemaphoreAcquireUnblocked":
            case "SemaphoreTryAcquire":
            case "SemaphoreRelease":
                ctx!.links.current.push({
                    source: event,
                    target: ctx!.objects[event.event.data.objectId],
                });
                break;
        }
        updateTimelineLinks();
    } else {
        d3.select("#sidebar-view-1").classed("show-event", false);
    }
}

function selectTaskImpl(taskId: TaskId | null, doSendMessage: boolean, openTab: boolean) {
    ctx!.timeline.spans!
        .classed("selected", false);
    ctx!.selection.taskId = taskId;
    ctx!.timeline.spans!
        .filter(d => d.isTask && (d.entry as Task).id === ctx!.selection.taskId)
        .classed("selected", true);
    if (taskId !== null) {
        if (doSendMessage) {
            sendMessage({ type: "taskClick", data: taskId });
        }

        // select tab
        if (openTab) {
            (d3.select(".panel-sidebar vscode-panels").node()! as any).activeid = "sidebar-tab-1";
        }

        // fill in sidebar
        const task = ctxData!.tasks[taskId];
        const infoRoot = d3.select("#sidebar-view-1")
            .classed("show-task", true)
            .select(".task-info");
        infoRoot.select(".task-info-id").text(taskId);
        infoRoot.select(".task-info-kind").text(task.isFuture ? "Future" : "Thread");
        infoRoot.select(".task-info-name").text(task.name !== null ? task.name : "-");
        if (task.id === 0) {
            infoRoot.select(".task-info-created").text("-");
        } else {
            const cell = infoRoot
                .select(".task-info-created")
                .html(`<vscode-link>Task ${task.createdBy}</vscode-link>, <vscode-link>Step ${task.createdAt}</vscode-link>`);
            cell.select("vscode-link:nth-of-type(1)")
                .on("click", () => selectTask(task.createdBy, true));
            cell.select("vscode-link:nth-of-type(2)")
                .on("click", () => selectEvent(task.createdAt, true));
        }
        infoRoot
            .select(".task-info-first")
            .html(`<vscode-link>Step ${task.firstStep}</vscode-link>`)
            .select("vscode-link")
            .on("click", () => selectEvent(task.firstStep, true));
        infoRoot
            .select(".task-info-last")
            .html(`<vscode-link>Step ${task.lastStep}</vscode-link>`)
            .select("vscode-link")
            .on("click", () => selectEvent(task.lastStep, true));

        // show links
        ctx!.links.current.push({
            source: ctx!.tasks[task.id],
            target: ctx!.events[task.createEvent!.id],
        });
        updateTimelineLinks();
    } else {
        d3.select("#sidebar-view-1").classed("show-task", false);
    }
}

function selectObjectImpl(objectId: ObjectId | null, doSendMessage: boolean, openTab: boolean) {
    ctx!.timeline.spans!
        .classed("selected", false);
    ctx!.selection.objectId = objectId;
    ctx!.timeline.spans!
        .filter(d => !d.isTask && (d.entry as Object).id === ctx!.selection.objectId)
        .classed("selected", true);
    if (objectId !== null) {
        if (doSendMessage) {
            sendMessage({ type: "objectClick", data: objectId });
        }

        // select tab
        if (openTab) {
            (d3.select(".panel-sidebar vscode-panels").node()! as any).activeid = "sidebar-tab-1";
        }

        // fill in sidebar
        const object = ctxData!.objects[objectId];
        const infoRoot = d3.select("#sidebar-view-1")
            .classed("show-object", true)
            .select(".object-info");
        infoRoot.select(".object-info-id").text(objectId);
        infoRoot.select(".object-info-kind").text(object.kind !== null ? object.kind : "Batch semaphore");
        infoRoot.select(".object-info-name").text(object.name !== null ? object.name : "-");
        const cell = infoRoot
            .select(".object-info-created")
            .html(`<vscode-link>Task ${object.createdBy}</vscode-link>, <vscode-link>Step ${object.firstStep}</vscode-link>`);
        cell.select("vscode-link:nth-of-type(1)")
            .on("click", () => selectTask(object.createdBy, true));
        cell.select("vscode-link:nth-of-type(2)")
            .on("click", () => selectEvent(object.firstStep, true)); // TODO: unnecessary?
        // TODO: object-info-seen
        infoRoot
            .select(".object-info-first")
            .html(`<vscode-link>Step ${object.firstStep}</vscode-link>`)
            .select("vscode-link")
            .on("click", () => selectEvent(object.firstStep, true));
        infoRoot
            .select(".object-info-last")
            .html(`<vscode-link>Step ${object.lastStep}</vscode-link>`)
            .select("vscode-link")
            .on("click", () => selectEvent(object.lastStep, true));

        // filter setup
        infoRoot.select(".object-create-filter")
            .on("click", () => sendMessage({ type: "createObjectFilterStart", data: objectId }));

        // show links
        ctx!.links.current.push({
            source: ctx!.objects[object.id],
            target: ctx!.events[object.createEvent!.id],
        });
        updateTimelineLinks();
    } else {
        d3.select("#sidebar-view-1").classed("show-object", false);
    }
}

/**
 * Select the given event.
 * @param sendMessage Should the backend be notified?
 */
export function selectEvent(eventId: EventId, sendMessage: boolean) {
    deselect();
    selectEventImpl(eventId, sendMessage, true);
}

/**
 * Select the given task.
 * @param sendMessage Should the backend be notified?
 */
export function selectTask(taskId: TaskId, sendMessage: boolean) {
    deselect();
    selectTaskImpl(taskId, sendMessage, true);
}

/**
 * Select the given object.
 * @param sendMessage Should the backend be notified?
 */
export function selectObject(objectId: ObjectId, sendMessage: boolean) {
    deselect();
    selectObjectImpl(objectId, sendMessage, true);
}

/**
 * Make sure the same item is selected, e.g., after updating filters.
 */
export function reselect() {
    if (ctx!.selection.eventId !== null) {
        selectEventImpl(ctx!.selection.eventId, false, false);
    } else if (ctx!.selection.taskId !== null) {
        selectTaskImpl(ctx!.selection.taskId, false, false);
    } else if (ctx!.selection.objectId !== null) {
        selectObjectImpl(ctx!.selection.objectId, false, false);
    }
}

/**
 * Deselect the currently selected task, event, or object.
 */
export function deselect() {
    // remove links
    ctx!.links.current.length = 0;
    updateTimelineLinks();

    // deselect rects
    selectEventImpl(null, false, false);
    selectTaskImpl(null, false, false);
    selectObjectImpl(null, false, false);
}
