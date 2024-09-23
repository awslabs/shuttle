import * as d3 from "d3";

import { ExtContextData, ExtContextFilters, ExtContextHierarchy } from "../common/context.mts";
import { Filter, filterByCategory, initFilter } from "../common/filters.mts";
import { eventCategories } from "../common/events.mts";
import { Task } from "../common/tasks.mts";
import { Object } from "../common/objects.mts";
import { createContextUISelection, deselect, ExtContextUISelection, reselect, selectEvent, selectObject, selectTask } from "./selection.mts";
import { createContextUIZoom, ExtContextUIZoom, updateClipHorizontal, updateZoomTrees } from "./zoom.mts";
import { createEventUI, EventUI } from "./events.mts";
import { createObjectUI, createTaskUI, HierarchyEntryUI, ObjectUI, TaskUI } from "./hierarchy.mts";
import { updateCdag } from "./cdag.mts";
import { ExtContextUITimeline, updateTimeline } from "./timeline.mts";
import { sendMessage } from "./main.mts";

// dimension constants
export const rowHeight = 17;
export const leftPanelWidth = 100;
export const marginTop = 20;
export const eventHeight = 13;

const minEventsOnScreen = 4;
const marginRight = 20;
const depthOffset = 10;

export let ctxData: ExtContextData | null = null;
export let ctxFilters: ExtContextFilters | null = null;
export let ctxHierarchy: ExtContextHierarchy | null = null;
export let ctx: ExtContextUI | null = null;

export type ExtContextUI = {
    width: number,
    height: number,
    root: d3.Selection<d3.BaseType, unknown, any, null>,
    events: EventUI[],
    tasks: TaskUI[],
    objects: ObjectUI[],
    hierarchy: HierarchyEntryUI[],
    timeline: ExtContextUITimeline,
    zoom: ExtContextUIZoom,
    main: {
        root: d3.Selection<d3.BaseType, unknown, any, null>,
    },
    left: {
        root: d3.Selection<SVGSVGElement, unknown, HTMLElement, any>,
    },
    right: {
        scrollRoot: d3.Selection<HTMLDivElement, unknown, any, any>,
        root: d3.Selection<SVGSVGElement, unknown, HTMLElement, any>,
    },
    links: {
        current: {
            source: HierarchyEntryUI | EventUI,
            target: HierarchyEntryUI | EventUI,
        }[],
        root: d3.Selection<d3.BaseType, unknown, HTMLElement, any> | null,
        pathRoot: d3.Selection<SVGGElement, unknown, HTMLElement, null> | null,
        entryRoot: d3.Selection<SVGGElement, unknown, HTMLElement, null> | null,
    },
    selection: ExtContextUISelection,
};

// TODO: rename function?
function updateFilters() {
    // re-number events based on filtered events
    let keptIdx = 0;
    for (let event of ctx!.events) {
        event.filtered = false;
        event.keptIdx = keptIdx;
        for (const filter of ctxFilters!.chain) {
            if (filter.enabled && !filter.check(filter.data, event.event)) {
                event.filtered = true;
                break;
            }
        }
        if (!event.filtered) {
            keptIdx++;
        }
    }
    ctxFilters!.keptEvents = keptIdx;

    updateCdag(ctx!);

    // update scale
    ctx!.timeline.shownEvents = ctx!.timeline.showingCdag ? ctx!.timeline.cdagColumns : ctxFilters!.keptEvents;
    ctx!.timeline.x.domain([0, ctx!.timeline.shownEvents]);
    ctx!.timeline.eventWidth = ctx!.timeline.xScaled(1) - ctx!.timeline.xScaled(0);

    for (const event of ctx!.events) {
        event.shownIdx = ctx!.timeline.showingCdag ? event.cdagKeptIdx : event.keptIdx;
    }

    updateZoomTrees(ctx!);
}

function updateFilterList() {
    d3.select("#filters-user")
        .selectAll("vscode-checkbox")
        .data(ctxFilters!.chain.filter(({builtin}) => !builtin))
        .join("vscode-checkbox")
            .attr("checked", ({enabled}) => enabled)
            .html(({title, totalCount}) => `${title} <vscode-badge>${totalCount}</vscode-badge> <i class="codicon codicon-trash"></i>`)
            .on("change", (ev, filter) => {
                filter.enabled = ev.target.checked;
                updateFilters();
                reselect();
                const zoomToMinEvents = ctx!.timeline.shownEvents / minEventsOnScreen;
                ctx!.zoom.zoom!.scaleExtent([1, Math.max(zoomToMinEvents, 1)]);
                updateZoom(null);
            })
            .select(".codicon-trash")
                .on("click", (_ev, filter) => {
                    const index = ctxFilters!.chain.indexOf(filter);
                    if (index !== -1) {
                        ctxFilters!.chain.splice(index, 1);
                        updateFilterList();
                        reselect();
                        const zoomToMinEvents = ctx!.timeline.shownEvents / minEventsOnScreen;
                        ctx!.zoom.zoom!.scaleExtent([1, Math.max(zoomToMinEvents, 1)]);
                        updateZoom(null);
                    }
                });
    updateFilters();
}

export function addCustomFilter(title: string, source: string) {
    try {
        const evalResult = eval(source);
        let init = null;
        let check = null;
        // TODO: a bit naive
        if (typeof evalResult === "function") {
            check = evalResult;
        } else if (typeof evalResult === "object") {
            if ("init" in evalResult) { init = evalResult.init; }
            if ("check" in evalResult) { check = evalResult.check; }
        } else {
            throw "unknown return type";
        }
        if (!check) {
            check = () => true;
        }
        addFilter(initFilter(ctxData!, {
            title,
            enabled: true,
            builtin: false,
            data: {},
            init,
            check,
        }), true);
    } catch (error) {
        // TODO: toast error to user
        console.log("custom filter error", error);
    }
}

export function addFilter(filter: Filter<any>, forceUpdate: boolean) {
    let exists = false;
    for (let i = 0; i < ctxFilters!.chain.length; i++) {
        if (!filter.builtin && ctxFilters!.chain[i].title === filter.title) {
            exists = true;
            if (forceUpdate) {
                ctxFilters!.chain[i].init = filter.init;
                ctxFilters!.chain[i].check = filter.check;
            }
            break;
        }
    }
    if (!exists) {
        // reveal the filters panel
        (d3.select(".panel-sidebar vscode-panels").node()! as any).activeid = "sidebar-tab-2";

        ctxFilters!.chain.push(filter);
        // TODO: this is duplicated a couple of times, move into a function:
        updateFilterList();
        reselect();
        const zoomToMinEvents = ctx!.timeline.shownEvents / minEventsOnScreen;
        ctx!.zoom.zoom!.scaleExtent([1, Math.max(zoomToMinEvents, 1)]);
        updateZoom(null);
    } else if (forceUpdate) {
        // TODO: this is duplicated a couple of times, move into a function:
        updateFilterList();
        reselect();
        const zoomToMinEvents = ctx!.timeline.shownEvents / minEventsOnScreen;
        ctx!.zoom.zoom!.scaleExtent([1, Math.max(zoomToMinEvents, 1)]);
        updateZoom(null);
    }
}

function updateZoom(ev: {transform: d3.ZoomTransform} | null) {
    if (ev) {
        ctx!.zoom.lastZoomTransform = ev.transform;
    }
    if (ctx!.zoom.lastZoomTransform !== null) {
        ctx!.timeline.xScaled = ctx!.zoom.lastZoomTransform.rescaleX(ctx!.timeline.x);
    }
    const xAxisTicks = ctx!.timeline.xScaled.ticks().filter(Number.isInteger);
    ctx!.timeline.xAxis.tickValues(xAxisTicks);
    ctx!.timeline.eventWidth = ctx!.timeline.xScaled(1) - ctx!.timeline.xScaled(0);
    ctx!.timeline.xAxisEl!.call(ctx!.timeline.xAxis.scale(ctx!.timeline.xScaled));
    updateClipHorizontal(ctx!);
    updateTimeline(true);
}

function updateHeight() {
    let visibleNodes = 1;
    if (ctx!.hierarchy.length > 0) {
        visibleNodes = ctx!.hierarchy[ctx!.hierarchy.length - 1].visibleIndex + 1;
    }
    ctx!.height = (visibleNodes + 1) * rowHeight;
    // TODO: changing viewbox messes with the animations: only change it after the transition
    ctx!.left.root
        .attr("viewBox", [0, 0, leftPanelWidth, ctx!.height + marginTop - 10]);
    ctx!.right.root
        .attr("viewBox", [0, 0, ctx!.width - marginRight - leftPanelWidth, ctx!.height + marginTop - 10]);
}

// TODO: move to hierarchy.mts
function updateLeftPanel() {
    const durationToggleEntry = 200;

    // links joining entries
    ctx!.links.pathRoot!
        .selectAll("path")
        .data(ctx!.hierarchy)
        .join("path")
            .transition()
                .duration(durationToggleEntry)
                .attr("d", ({visibleIndex, entry: {createdBy}}) => visibleIndex === 0 ? "" :
                    `M${ctxData!.tasks[createdBy].depth * depthOffset},${marginTop + ctx!.tasks[createdBy].visibleIndex * rowHeight}`
                    + ` V${marginTop + visibleIndex * rowHeight}`
                    + ` h${depthOffset}`)
                .attr("style", ({visible}) => `opacity: ${visible ? 1 : 0};`);

    // the entries themselves
    const hierarchyEntries = ctx!.links.entryRoot!
        .selectAll("g")
        .data(ctx!.hierarchy)
        .join("g");
    hierarchyEntries
        .transition()
            .duration(durationToggleEntry)
            .attr("transform", ({visibleIndex}) => `translate(0,${marginTop + visibleIndex * rowHeight})`)
            .attr("style", ({visible}) => visible ? "opacity: 1;" : "opacity: 0;")
            .on("start", () => { hierarchyEntries.filter(({visible}) => visible).attr("style", "display: block;"); })
            .on("end", () => { hierarchyEntries.filter(({visible}) => !visible).attr("style", "display: none;"); });
    const entryCircle = hierarchyEntries
        .selectAll("circle")
        .data(d => [d])
        .join("circle")
            .attr("cx", ({entry: {depth}}) => depth * depthOffset)
            .attr("r", 4.5)
            .attr("fill", ({entry: {children}}) => children.length ? null : "#999");
    const entryPM = hierarchyEntries
        .selectAll("text.pm")
        .data(d => d.entry.children.length ? [d] : [])
        .join("text")
            .classed("pm", true)
            .attr("stroke", "#fff")
            .attr("transform", ({entry: {depth}}) => `translate(${-3 + depth * depthOffset},2.5)`)
            .text(({open}) => open ? "–" : "+");
    entryCircle
        .on("click", (_ev, entry) => {
            entry.open = !entry.open;
            updateVisible();
        });
    entryPM // TODO: avoid this duplicate listener
        .on("click", (_ev, entry) => {
            entry.open = !entry.open;
            updateVisible();
        });
    hierarchyEntries
        .selectAll("text.entry")
        .data(d => [d])
        .join("text")
            .classed("entry", true)
            .attr("dy", "0.32em")
            .attr("x", ({entry: {depth}}) => depth * depthOffset + 8)
            .text(d => d.entry.name !== null ? d.entry.name : (d.isTask ? `task ${(d.entry as Task).id}` : `object ${(d.entry as Object).id}`))
            .on("click", (_ev, d) => d.isTask ? selectTask((d.entry as Task).id, true) : selectObject((d.entry as Object).id, true));
}

function updateVisible() {
    let visibleIndex = -1;
    function walk(entry: HierarchyEntryUI, visible: boolean) {
        if (!entry.isTask && !ctx!.timeline.showingObjects) {
            visible = false;
        }
        entry.visible = visible;
        entry.visibleIndex = visible ? ++visibleIndex : visibleIndex;
        entry.entry.children.forEach((child) => walk(ctx!.hierarchy[child.flatIndex], visible && entry.open));
    }
    walk(ctx!.hierarchy[0], true);
    updateHeight();
    updateTimeline(false);
    updateLeftPanel();
}

function updateCdagToggle() {
    let cdag = ctx!.timeline.showingCdag;
    for (let event of ctx!.events) {
        event.shownIdx = cdag ? event.cdagKeptIdx : event.keptIdx;
    }
    ctx!.timeline.shownEvents = cdag ? ctx!.timeline.cdagColumns : ctxFilters!.keptEvents;
    d3.select("#toggle-cdag").classed("selected", cdag);
}

export function createContextUI(
    argCtxData: ExtContextData,
    argCtxFilters: ExtContextFilters,
    argCtxHierarchy: ExtContextHierarchy,
): ExtContextUI {
    // are we setting up for the first time?
    const firstSetup = !ctx;

    ctxData = argCtxData;
    ctxFilters = argCtxFilters;
    ctxHierarchy = argCtxHierarchy;

    const width = (d3.select("main div.panel-main").node()! as HTMLDivElement).clientWidth;
    const x = d3.scaleLinear([0, ctxFilters!.keptEvents], [0, width - marginRight - leftPanelWidth]);

    // create/get DOM roots
    const root = d3.select("main");

    // root: top panel (X axis)
    const rootTop = d3.select<SVGSVGElement, unknown>("svg.panel-top")
        .attr("width", width - marginRight - leftPanelWidth)
        .attr("height", marginTop)
        .attr("viewBox", [0, 0, width - marginRight - leftPanelWidth, marginTop])
        .attr("style", "max-width: 100%; height: auto; font: 10px sans-serif; overflow: visible;")
        .html("");

    // root: main panel (hierarchy and timeline)
    const rootMain = root.select("div.panel-main");
    rootMain.html("");

    const scrollHorizontal = rootMain.append("div")
        .classed("panel-main-right", true);
    const rootRight = scrollHorizontal.append("svg")
        .attr("width", width - marginRight - leftPanelWidth)
        .attr("style", "height: auto; font: 10px sans-serif; overflow: hidden;");

    const tasksUI = ctxData.tasks.map(createTaskUI);
    const objectsUI = ctxData.objects.map(createObjectUI);
    const hierarchyUI = ctxHierarchy.hierarchy.map(e => e.isTask ? tasksUI[(e as Task).id] : objectsUI[(e as Object).id]);

    // keep hierarchy open to a max depth of 3 by default
    let visibleIndex = -1;
    function walk(entry: HierarchyEntryUI, depth: number): void {
        entry.visible = depth <= 3;
        entry.visibleIndex = entry.visible ? ++visibleIndex : visibleIndex;
        entry.entry.children.forEach((child) => walk(hierarchyUI[child.flatIndex], depth + 1));
    }
    if (hierarchyUI.length > 0) {
        walk(hierarchyUI[0], 0);
    }

    ctx = {
        width,
        height: 1,
        root,
        events: ctxData.events.map(createEventUI),
        tasks: tasksUI,
        objects: objectsUI,
        hierarchy: hierarchyUI,
        timeline: {
            x,
            xScaled: x,
            xAxis: d3.axisTop(x)
                .tickFormat(d3.format("d")),
            xAxisEl: null,
            eventWidth: x(1) - x(0),
            root: rootRight.append("svg:g")
                .attr("transform", `translate(0,${marginTop})`),
            rows: null,
            spans: null,
            events: null,
            showingCdag: false,
            cdagColumns: 0,
            shownEvents: ctxFilters!.keptEvents,
            showingObjects: true,
        },
        zoom: createContextUIZoom(),
        main: {
            root: rootMain,
        },
        left: {
            root: rootMain.append("svg")
                .attr("width", leftPanelWidth)
                .attr("style", "height: auto; font: 10px sans-serif; overflow: hidden;")
                .classed("panel-main-left", true),
        },
        right: {
            scrollRoot: scrollHorizontal,
            root: rootRight,
        },
        links: {
            current: [],
            root: null,
            pathRoot: null,
            entryRoot: null,
        },
        selection: createContextUISelection(),
    };

    updateHeight();
    updateFilters();
    updateCdagToggle();

    // create arrow tip
    ctx!.right.root.append("svg:defs").append("svg:marker")
        .attr("id", "arrow")
        .attr("viewBox", [0, 0, 10, 10])
        .attr("refX", 5)
        .attr("refY", 5)
        .attr("markerWidth", 6)
        .attr("markerHeight", 6)
        .attr("orient", "auto-start-reverse")
        .append("svg:path")
            .attr("d", "M 0 0 L 10 5 L 0 10 z");

    // create X axis for time (in samples)
    ctx!.timeline.xAxisEl = rootTop.append("g")
        .attr("transform", `translate(0,${marginTop})`)
        .call(ctx!.timeline.xAxis);

    // create event timeline
    let timelineMain = ctx!.right.root.append("g")
        .attr("transform", `translate(0,${marginTop})`);

    // background to catch zoom and drag events
    timelineMain.append("svg:rect")
        .attr("transform", `translate(0,-${marginTop})`)
        .attr("width", "2000")
        .attr("height", "2000")
        .attr("fill", "transparent")
        .on("click", deselect);

    // setup zoom
    const zoomToMinEvents = ctx!.timeline.shownEvents / minEventsOnScreen;
    ctx!.zoom.zoom = d3.zoom<SVGGElement, any>()
        .scaleExtent([1, Math.max(zoomToMinEvents, 1)])
        .translateExtent([[0, 0], [width - marginRight - leftPanelWidth, 0]])
        // prevent scrolling then apply the default filter
        .filter((event: MouseEvent): boolean => {
            event.preventDefault();
            return (!event.ctrlKey || event.type === 'wheel') && !event.button;
        })
        .on("zoom", updateZoom);
    timelineMain.call(ctx!.zoom.zoom!);
    updateClipHorizontal(ctx!);

    // timeline rows
    ctx!.timeline.rows = timelineMain
        .selectAll("g.tl-row")
        .data(ctx!.hierarchy)
        .join("svg:g")
            .classed("tl-row", true);

    // show task/object spans
    ctx!.timeline.spans = ctx!.timeline.rows
        .selectAll("rect.to-span")
        .data(d => [d])
        .join("svg:rect")
            .attr("height", `${eventHeight + 4}px`)
            .classed("to-span", true)
            .classed("task", ({isTask}) => isTask)
            .classed("object", ({isTask}) => !isTask)
            .on("click", (_ev, d) => d.isTask
                ? selectTask((d.entry as Task).id, true)
                : selectObject((d.entry as Object).id, true));
    ctx!.links.root = timelineMain.append("svg:g")
        .classed("tl-links", true);
    updateTimeline(true);

    // select event zero
    selectEvent(0, false);

    // left panel: hierarchy of tasks and objects
    ctx!.links.pathRoot = ctx!.left.root.append("g")
        .attr("fill", "none")
        .attr("stroke", "#999")
        .attr("transform", `translate(15,0)`);
    ctx!.links.entryRoot = ctx!.left.root.append("g")
        .attr("transform", `translate(15,0)`);
    updateLeftPanel();

    if (firstSetup) {
        // change file icon to reload icon
        d3.select("#reload-schedule .codicon")
            .classed("codicon-file", false)
            .classed("codicon-refresh", true);

        // setup fit-to-width
        window.addEventListener("resize", () => {
            // TODO: throttle (only set one?)
            requestAnimationFrame(() => {
                ctx!.width = (ctx!.main.root.node()! as HTMLDivElement).clientWidth;
                x.range([0, ctx!.width - marginRight - leftPanelWidth]);
                ctx!.zoom.zoom!.translateExtent([[0, 0], [ctx!.width - marginRight - leftPanelWidth, 0]]);
                rootTop
                    .attr("width", ctx!.width - marginRight - leftPanelWidth)
                    .attr("viewBox", [0, 0, ctx!.width - marginRight - leftPanelWidth, marginTop]);
                ctx!.right.root
                    .attr("width", ctx!.width - marginRight - leftPanelWidth);
                updateZoom(null);
                updateHeight();
            });
        });

        // setup keyboard navigation
        window.addEventListener("keydown", (ev) => {
            switch (ev.code) {
                case "ArrowLeft":
                case "ArrowRight":
                    const left = ev.code === "ArrowLeft";
                    if (ctx!.timeline.shownEvents > 0) {
                        // select leftmost/rightmost visible event
                        let [startScan, endScan, deltaScan] = left ? [ctx!.events.length - 1, -1, -1] : [0, ctx!.events.length, 1];
                        if (ctx!.selection.eventId !== null) {
                            // select next visible event to the left/right
                            startScan = ctx!.selection.eventId + deltaScan;
                        }
                        for (let eventId = startScan; eventId !== endScan; eventId += deltaScan) {
                            if (!ctx!.events[eventId].filtered) {
                                selectEvent(eventId, true);
                                break;
                            }
                        }
                    }
                    break;
                default:
                    return;
            }
            ev.preventDefault();
        });
    }

    // filter checkboxes, badges
    for (let category of eventCategories) {
        d3.select(`#filters-${category}`)
            .on("change", (ev) => {
                filterByCategory.data.enabledCategories[category] = ev.target.checked;

                // TODO: duplication
                updateFilters();
                reselect();
                const zoomToMinEvents = ctx!.timeline.shownEvents / minEventsOnScreen;
                ctx!.zoom.zoom!.scaleExtent([1, Math.max(zoomToMinEvents, 1)]);
                updateZoom(null);
            });
        d3.select(`#filters-${category} vscode-badge`).text(filterByCategory.data.categoryCounts[category]!);
    }
    d3.select("#filters-user").html("");
    d3.select("#filters-add")
        .on("click", (_ev) => sendMessage({type: "createCustomFilter"}));

    // causal graph toggle
    d3.select("#toggle-cdag")
        .on("click", (_ev) => {
            ctx!.timeline.showingCdag = !ctx!.timeline.showingCdag;
            updateCdagToggle();

            // TODO: duplication
            updateFilters();
            reselect();
            const zoomToMinEvents = ctx!.timeline.shownEvents / minEventsOnScreen;
            ctx!.zoom.zoom!.scaleExtent([1, Math.max(zoomToMinEvents, 1)]);
            updateZoom(null);
        });

    // object toggle
    d3.select("#toggle-objects")
        .on("click", (_ev) => {
            ctx!.timeline.showingObjects = !ctx!.timeline.showingObjects;
            d3.select("#toggle-objects").classed("selected", ctx!.timeline.showingObjects);
            updateVisible();
        });

    return ctx;
}
