import { ExtContextData } from "./context.mts";
import { HierarchyEntry } from "./hierarchy.mts";
import { ScheduleObject } from "./schedule.mts";

export type ObjectId = number;

export type Object = HierarchyEntry & {
    source: ScheduleObject,
    id: ObjectId,
    isTask: false,
    seenBy: Map<number, boolean>,
    kind: string | null,
};

/**
 * Reads the given object from the annotated schedule.
 */
export function readObject(ctx: ExtContextData, objectId: number): Object {
    const {created_by, created_at, name, kind} = ctx.source.objects[objectId];
    const object: Object = {
        source: ctx.source.objects[objectId],
        id: objectId,
        isTask: false,
        name,
        kind,
        createEvent: null,
        createdBy: created_by,
        firstStep: created_at,
        createdAt: created_at,
        lastStep: ctx.source.events.length - 1,
        seenBy: new Map(),
        children: [],
        path: [],
        flatIndex: 0,
        depth: 0,
        events: [],
    };
    return object;
}

export function writeObject(object: Object): ScheduleObject {
    return {
        created_by: object.createdBy,
        created_at: object.createdAt,
        name: object.name,
        kind: object.kind,
    };
}
