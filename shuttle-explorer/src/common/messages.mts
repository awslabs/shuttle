import { EventId } from "./events.mts";
import { ObjectId } from "./objects.mts";
import { Schedule } from "./schedule.mts";
import { TaskId } from "./tasks.mts";

/**
 * Message sent from the backend to the frontend.
 */
export type MessageBackToFront =
    {
        type: "createTimeline",
        data: Schedule,
    } | {
        type: "createEventFilter",
        data: {
            eventId: EventId,
            kind: "before" | "after" | "concurrent",
        },
    } | {
        type: "createObjectFilter",
        data: {
            objectId: ObjectId,
            keepObject: boolean,
        },
    } | {
        type: "updateCustomFilter",
        data: {
            name: string,
            source: string,
        },
    };

/**
 * Message sent from the frontend to the backend.
 */
export type MessageFrontToBack =
    {
        type: "backtraceClick",
        data: [EventId, number],
    } | {
        type: "createCustomFilter",
    } | {
        type: "createEventFilterStart",
        data: EventId,
    } | {
        type: "createObjectFilterStart",
        data: ObjectId,
    } | {
        type: "eventHover",
        data: EventId,
    } | {
        type: "eventHoverOff",
        data: EventId,
    } | {
        type: "eventClick",
        data: EventId,
    } | {
        type: "objectClick",
        data: ObjectId,
    } | {
        type: "reloadSchedule",
    } | {
        type: "taskClick",
        data: TaskId,
    };
