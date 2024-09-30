import * as d3 from "d3";

import { Object } from "../common/objects.mts";
import { Task } from "../common/tasks.mts";
import { EventUI } from "./events.mts";
import { HierarchyEntryUI } from "./hierarchy.mts";
import { ctx, ctxData, eventHeight, rowHeight } from "./ui.mts";
import { getZoomTrees, minEventWidth, ZoomTree } from "./zoom.mts";
import { selectEvent } from "./selection.mts";
import { clockCmp, Ordering } from "../common/clocks.mts";

const linkGen = d3.link(d3.curveBumpY);

export type ExtContextUITimeline = {
    x: d3.ScaleLinear<number, number>,
    xScaled: d3.ScaleLinear<number, number>,
    xAxis: d3.Axis<d3.NumberValue>,
    xAxisEl: d3.Selection<SVGGElement, unknown, HTMLElement, null> | null,
    eventWidth: number,
    root: d3.Selection<d3.BaseType, unknown, any, null>,
    rows: d3.Selection<d3.BaseType, HierarchyEntryUI, d3.BaseType, null> | null,
    spans: d3.Selection<d3.BaseType, HierarchyEntryUI, d3.BaseType, HierarchyEntryUI> | null,
    events: d3.Selection<d3.BaseType, ZoomTree<EventUI>, d3.BaseType, HierarchyEntryUI> | null,
    showingCdag: boolean,
    cdagColumns: number,
    shownEvents: number,
    showingObjects: boolean,
};

export function updateTimeline(updateEvents: boolean) {
    updateTimelineRows();
    if (updateEvents) {
        updateTimelineEvents();
        updateTimelineSpans();
        updateTimelineBlocked();
    }
    updateTimelineLinks();
}

function updateTimelineRows() {
    ctx!.timeline.rows!
        .transition()
            .duration(200)
            .attr("transform", ({visibleIndex}) => `translate(0,${visibleIndex * rowHeight - 9})`)
            .attr("style", ({visible}) => `opacity: ${visible ? 1 : 0};`);
}

function updateTimelineSpans() {
    const leftX = d3.local<number>();
    const rightX = d3.local<number>();
    ctx!.timeline.spans!
        .classed("selected", d => d.isTask
            ? ctx!.selection.taskId === (d.entry as Task).id
            : ctx!.selection.objectId === (d.entry as Object).id)
        .each(function ({entry: {firstStep, lastStep}}) {
            leftX.set(this as Element, Math.max(ctx!.timeline.xScaled(ctx!.events[firstStep].shownIdx) - 2, -2));
            rightX.set(this as Element, Math.min(ctx!.timeline.xScaled(ctx!.events[lastStep].shownIdx + (ctx!.events[lastStep].filtered ? 0 : 1)) + 2, ctx!.width));
        })
        .attr("transform", function () { return `translate(${leftX.get(this as Element)},0)`; })
        .attr("width", function () {
            if (rightX.get(this as Element)! <= leftX.get(this as Element)!) { return "1px"; }
            return `${rightX.get(this as Element)! - leftX.get(this as Element)!}px`;
        });
}

function updateTimelineBlocked() {
    /*
    // TODO: disabled
    ctx!.timeline.rows!
        .selectAll("rect.blocked")
        .data(d => d.isTask ? (d as Task).blockedAt : [])
        .join("svg:rect")
            .classed("blocked", true)
            .attr("transform", (at) => `translate(${ctx!.timeline.xScaled(ctxData!.events[at].keptIdx) + 2},2)`)
            .attr("height", `${eventHeight}px`)
            .attr("width", "5px");
            */
}

export function updateTimelineLinks() {
    // TODO: toggle somehow
    /*
    ctx!.links.current.length = 0;
    for (let event of ctx!.events) {
        for (let prevEvent of event.causallyDependsOn) {
            ctx!.links.current.push({
                source: event,
                target: prevEvent,
            });
        }
    }
    */

    ctx!.links.root!
        .selectAll("path.link")
        .data(ctx!.links.current)
        .join("path")
            .classed("link", true)
            .attr("d", ({source, target}) => {
                if (!source || !target) { return ""; }
                let sourceEvent = "isTask" in source ? ctx!.events[source.entry.firstStep] : source;
                let targetEvent = "isTask" in target ? ctx!.events[target.entry.firstStep] : target;
                let sourceTask = "isTask" in source ? source : ctx!.tasks[sourceEvent.event.taskId];
                let targetTask = "isTask" in target ? target : ctx!.tasks[targetEvent.event.taskId];
                if (!sourceTask.visible || !targetTask.visible) { return ""; }
                return linkGen({
                    source: [ctx!.timeline.xScaled(sourceEvent.shownIdx) + Math.min(ctx!.timeline.eventWidth, 4), sourceTask.visibleIndex * rowHeight],
                    target: [ctx!.timeline.xScaled(targetEvent.shownIdx) + Math.min(ctx!.timeline.eventWidth, 4), targetTask.visibleIndex * rowHeight],
                });
            });
}

export function updateTimelineEvents() {
    ctx!.timeline.events = ctx!.timeline.rows!
        .selectAll(".event")
        .data(({entry: {flatIndex}, visible}) => visible && ctx!.zoom.trees[flatIndex] !== null ? getZoomTrees(ctx!, ctx!.zoom.trees[flatIndex][0][0]) : [])
        .join("svg:rect")
            .attr("transform", ({minData}) => `translate(${ctx!.timeline.xScaled(minData.shownIdx)},2)`)
            .attr("height", `${eventHeight}px`)
            // TODO: don't apply max if the event is the right-most in its task to avoid visually overflowing the task row
            .attr("width", ({minData, maxData}) => `${Math.max(minEventWidth, ctx!.timeline.xScaled(maxData.shownIdx + 1) - ctx!.timeline.xScaled(minData.shownIdx))}px`)
            .classed("event", true)
            .classed("summary", ({count}) => count > 1)
            .classed("selected", ({minData, maxData}) => ctx!.selection!.eventId !== null
                && minData.event.taskId === ctxData!.events[ctx!.selection!.eventId].taskId
                && minData.event.id <= ctx!.selection!.eventId
                && ctx!.selection!.eventId <= maxData.event.id)
            .classed("before", ({maxData}) => ctx!.selection!.eventId !== null
                && maxData.event.id < ctx!.selection!.eventId
                && (maxData.event.taskId === ctxData!.events[ctx!.selection!.eventId].taskId
                    || clockCmp(maxData.event, ctxData!.events[ctx!.selection!.eventId]) === Ordering.Less))
            .classed("after", ({minData}) => ctx!.selection!.eventId !== null
                && ctx!.selection!.eventId < minData.event.id
                && (minData.event.taskId === ctxData!.events[ctx!.selection!.eventId].taskId
                    || clockCmp(ctxData!.events[ctx!.selection!.eventId], minData.event) === Ordering.Less))
            .on("click", (_ev, {keptData}) => { if (keptData !== null) selectEvent(keptData.event.id, true); });
            /*
            .on("mouseover", (_ev, {id}) => sendMessage({ type: "eventHover", data: id }))
            .on("mouseout", (_ev, {id}) => sendMessage({ type: "eventHoverOff", data: id }))
            * /;
/*
    ctx!.timeline.events = ctx!.timeline.rows!
        .selectAll(".event")
        .data(({entry: {events}}) => events.map(({id}) => ctx!.events[id]).filter(({filtered}) => !filtered))
        .join("svg:rect")
            .attr("transform", ({shownIdx}) => `translate(${ctx!.timeline.xScaled(shownIdx)},2)`)
            .attr("height", `${eventHeight}px`)
            .attr("width", `${ctx!.timeline.eventWidth}px`)
            .classed("event", true)
            .classed("selected", ({event: {id}}) => ctx!.selection.eventId === id)
            .on("mouseover", (_ev, {event: {id}}) => sendMessage({ type: "eventHover", data: id }))
            .on("mouseout", (_ev, {event: {id}}) => sendMessage({ type: "eventHoverOff", data: id }))
            .on("click", (_ev, {event: {id}}) => selectEvent(id, true));
            */
}
