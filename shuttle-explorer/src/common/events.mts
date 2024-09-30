import { ExtContextData } from "./context.mts";
import { ObjectId } from "./objects.mts";
import { ScheduleEvent } from "./schedule.mts";
import { TaskId } from "./tasks.mts";

export type EventCategory =
    "tick"
    | "semaphore"
    | "task"
    | "random";

export const eventCategories: EventCategory[] = [
    "tick",
    "semaphore",
    "task",
    "random",
];

export type EventBacktraceFrame = {
    btId: number,
    path: string,
    functionName: string,
    line: number,
    col: number,
};
export type EventBacktrace = EventBacktraceFrame[];

export type EventKind =
    | "SemaphoreCreated"
    | "SemaphoreClosed"
    | "SemaphoreAcquireFast"
    | "SemaphoreAcquireBlocked"
    | "SemaphoreAcquireUnblocked"
    | "SemaphoreTryAcquire"
    | "SemaphoreRelease"
    | "TaskCreated"
    | "TaskTerminated"
    | "Random"
    | "Tick";

export type EventId = number;

export type Event = {
    source: ScheduleEvent,
    id: EventId,
    taskId: TaskId,
    backtrace: EventBacktrace | null,
    kind: EventKind | null,
    category: EventCategory | null,
    data: any,
    clock: number[] | null,
    runnable: TaskId[] | null,
    extra: any,
};

/**
 * Reads the given event from the annotated schedule. Tasks and objects should
 * be initialised in `ctx` by this point.
 */
export function readEvent(ctx: ExtContextData, eventId: number): Event {
    const [taskId, info, kind, clock, runnable, extra] = ctx.source.events[eventId];
    const event: Event = {
        source: ctx.source.events[eventId],
        id: eventId,
        taskId,
        backtrace: null,
        kind: null,
        category: null,
        data: null,
        clock: !!clock ? clock : null,
        runnable: !!runnable ? runnable : null,
        extra,
    };
    if (info) {
        event.backtrace = info.map(([pathId, functionId, line, col], btId) => ({
            btId,
            path: ctx.source.files[pathId].path,
            functionName: ctx.source.functions[functionId].name,
            line,
            col,
        }));
    }

    // parse data into nicer representation
    if (typeof kind === "string" || kind instanceof String) {
        event.kind = kind as EventKind;
    } else {
        if ("TaskCreated" in kind) {
            event.kind = "TaskCreated";
            event.data = {
                taskId: kind["TaskCreated"][0],
                isFuture: kind["TaskCreated"][1],
            };
        } else if ("SemaphoreCreated" in kind) {
            event.kind = "SemaphoreCreated";
            event.data = {
                objectId: kind["SemaphoreCreated"],
            };
        } else if ("SemaphoreClosed" in kind) {
            event.kind = "SemaphoreClosed";
            event.data = {
                objectId: kind["SemaphoreClosed"],
            };
        } else if ("SemaphoreAcquireFast" in kind) {
            event.kind = "SemaphoreAcquireFast";
            event.data = {
                objectId: kind["SemaphoreAcquireFast"][0],
                numPermits: kind["SemaphoreAcquireFast"][1],
            };
        } else if ("SemaphoreAcquireBlocked" in kind) {
            event.kind = "SemaphoreAcquireBlocked";
            event.data = {
                objectId: kind["SemaphoreAcquireBlocked"][0],
                numPermits: kind["SemaphoreAcquireBlocked"][1],
            };
        } else if ("SemaphoreAcquireUnblocked" in kind) {
            event.kind = "SemaphoreAcquireUnblocked";
            event.data = {
                objectId: kind["SemaphoreAcquireUnblocked"][0],
                taskId: kind["SemaphoreAcquireUnblocked"][1],
                numPermits: kind["SemaphoreAcquireUnblocked"][2],
            };
        } else if ("SemaphoreTryAcquire" in kind) {
            event.kind = "SemaphoreTryAcquire";
            event.data = {
                objectId: kind["SemaphoreTryAcquire"][0],
                numPermits: kind["SemaphoreTryAcquire"][1],
                successful: kind["SemaphoreTryAcquire"][2],
            };
        } else if ("SemaphoreRelease" in kind) {
            event.kind = "SemaphoreRelease";
            event.data = {
                objectId: kind["SemaphoreRelease"][0],
                numPermits: kind["SemaphoreRelease"][1],
            };
        }
    }

    // identify tasks which have seen an object
    function seen(objectId: ObjectId, taskId: TaskId) {
        ctx.objects[objectId].seenBy.set(taskId, true);
    }

    let category: EventCategory | null = null;
    switch (event.kind) {
        case "TaskCreated":
            category = "task";
            // update task kind
            ctx.tasks[event.data.taskId].isFuture = event.data.isFuture;
            ctx.tasks[event.data.taskId].createEvent = event;
            ctx.tasks[event.data.taskId].createdAt = eventId;
            break;
        case "TaskTerminated":
            category = "task";
            // update task closing event
            ctx.tasks[taskId].lastStep = eventId;
            break;

        case "SemaphoreCreated":
            category = "semaphore";
            seen(event.data.objectId, taskId);
            ctx.objects[event.data.objectId].createEvent = event;
            break;
        case "SemaphoreClosed":
            category = "semaphore";
            seen(event.data.objectId, taskId);
            // update object closing event
            ctx.objects[event.data.objectId].lastStep = eventId;
            break;
        case "SemaphoreAcquireFast":
            category = "semaphore";
            seen(event.data.objectId, taskId);
            break;
        case "SemaphoreAcquireBlocked":
            category = "semaphore";
            seen(event.data.objectId, taskId);
            break;
        case "SemaphoreAcquireUnblocked":
            category = "semaphore";
            seen(event.data.objectId, taskId);
            break;
        case "SemaphoreTryAcquire":
            category = "semaphore";
            seen(event.data.objectId, taskId);
            break;
        case "SemaphoreRelease":
            category = "semaphore";
            seen(event.data.objectId, taskId);
            break;

        case "Tick":
            category = "tick";
            break;

        case "Random":
            category = "random";
            break;
    }
    event.category = category;

    // record which tasks were blocked
    if (event.runnable !== null) {
        for (const {id, firstStep, lastStep, blockedAt} of ctx.tasks) {
            if (!event.runnable.includes(id)
                && firstStep <= eventId
                && eventId <= lastStep) {
                blockedAt.push(eventId);
                // blockedRanges.push([lastSchedule, eventId]);
            }
        }
        // TODO: keeping track of blocked *ranges* rather than ticks should
        //       be more efficient
        // lastSchedule = eventId;
    }

    // compute events belonging to each task
    ctx.tasks[taskId].events.push(event);

    return event;
}

export function writeEvent(event: Event): ScheduleEvent {
    return [
        event.taskId,
        event.source[1],
        event.source[2],
        event.clock,
        event.runnable,
        event.extra,
    ];
}
