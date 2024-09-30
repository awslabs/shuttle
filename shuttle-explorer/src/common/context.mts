import { Event, eventCategories, EventId, readEvent } from "./events.mts";
import { Filter, filterByCategory, initFilter } from "./filters.mts";
import { HierarchyEntry } from "./hierarchy.mts";
import { Object, ObjectId, readObject } from "./objects.mts";
import { Schedule, ScheduleV0 } from "./schedule.mts";
import { Task, TaskId, readTask } from "./tasks.mts";

export type ExtContextData = {
    source: ScheduleV0,
    tasks: Task[],
    objects: Object[],
    events: Event[],
};

export type ExtContextHierarchy = {
    hierarchy: HierarchyEntry[],
};

export type ExtContextFilters = {
    chain: Filter<any>[],
    keptEvents: number,
};

export function createContextData(schedule: Schedule): ExtContextData {
    switch (schedule.version) {
        case 0:
            const scheduleV0: ScheduleV0 = (schedule as ScheduleV0);

            const ctx: ExtContextData = {
                source: scheduleV0,
                tasks: [],
                objects: [],
                events: [],
            };

            // process tasks, objects, then events; the order matters
            for (let taskId: TaskId = 0; taskId < scheduleV0.tasks.length; taskId++) {
                ctx.tasks.push(readTask(ctx, taskId));
            }
            for (let objectId: ObjectId = 0; objectId < scheduleV0.objects.length; objectId++) {
                ctx.objects.push(readObject(ctx, objectId));
            }
            for (let eventId: EventId = 0; eventId < scheduleV0.events.length; eventId++) {
                ctx.events.push(readEvent(ctx, eventId));
            }
            return ctx;
        default:
            throw "schedule version not supported";
    }
}

export function createContextHierarchy(ctxData: ExtContextData): ExtContextHierarchy {
    const ctx: ExtContextHierarchy = {
        hierarchy: [],
    };

    // heuristic: find nearest common ancestor for tasks that have seen the object
    for (const object of ctxData.objects) {
        const path = ctxData.tasks[object.createdBy].path;
        let pathLength = path.length;
        for (let seenById of [...object.seenBy.keys()].sort()) {
            let otherPath = ctxData.tasks[seenById].path;
            if (pathLength > otherPath.length) {
                pathLength = otherPath.length;
            }
            while (pathLength > 0 && path[pathLength - 1] !== otherPath[pathLength - 1]) {
                pathLength--;
            }
        }

        // TODO: heuristic: only keep objects that were seen by two or more threads
        // TODO: hackily only enabled for schedules with a large number of objects!
        if (ctxData.objects.length < 1000 || object.seenBy.size > 1) {
            ctxData.tasks[path[pathLength - 1]].children.push(object);
        }
    }

    // flatten data for hierarchy display
    let visibleIndex = -1;
    function walk(entry: HierarchyEntry, depth: number): void {
        // keep hierarchy open to a max depth of 3 by default
        //entry.visible = depth <= 3;
        //entry.visibleIndex = entry.visible ? ++visibleIndex : visibleIndex;
        entry.flatIndex = ctx.hierarchy.length;
        entry.depth = depth;
        ctx.hierarchy.push(entry);
        entry.children.forEach((child) => walk(child, depth + 1));
    }
    walk(ctxData.tasks[0], 0);

    return ctx;
}

export function createContextFilters(ctxData: ExtContextData): ExtContextFilters {
    let filter = initFilter(ctxData, filterByCategory);
    let keptEvents = 0;
    for (let category of eventCategories) {
        if (filter.data.enabledCategories[category]) {
            keptEvents += filter.data.categoryCounts[category];
        }
    }
    return {
        chain: [filter],
        keptEvents,
    };
}
