/**
 * Type definitions must match the JSON produced by `serde`.
 */

// TODO: name these types better

export type ScheduleObject = {
    created_by: number,
    created_at: number,
    name: string | null,
    kind: string | null,
};

export type ScheduleTask = {
    created_by: number,
    first_step: number,
    last_step: number,
    name: string | null,
};

export type ScheduleEventKind =
    | {"SemaphoreCreated": number}
    | {"SemaphoreClosed": number}
    | {"SemaphoreAcquireFast": [number, number]}
    | {"SemaphoreAcquireBlocked": [number, number]}
    | {"SemaphoreAcquireUnblocked": [number, number, number]}
    | {"SemaphoreTryAcquire": [number, number, boolean]}
    | {"SemaphoreRelease": [number, number]}
    | {"TaskCreated": [number, boolean]}
    | "TaskTerminated"
    | "Random"
    | "Tick"
    | {"Schedule": number[]};

export type ScheduleEvent = [
    number, // task
    [number, number, number, number][] | null, // backtrace
    ScheduleEventKind,
    number[] | null, // vector clock
    number[] | null, // runnable tasks when scheduling
    any, // optional extra data
];

export type Schedule = {version: number};

export type ScheduleV0 = {
    version: 0,
    files: {path: string}[],
    functions: {name: string}[],
    objects: ScheduleObject[],
    tasks: ScheduleTask[],
    events: ScheduleEvent[],
};

export type ProcessedSchedule = {
    previousEvents: ScheduleEvent[][],
};
