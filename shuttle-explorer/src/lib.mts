import {
    createContextData,
    createContextHierarchy,
    createContextFilters,
    ExtContextData,
    ExtContextHierarchy,
    ExtContextFilters,
} from "./common/context.mts";
import { Event, EventId, writeEvent } from "./common/events.mts";
import { Object, ObjectId, writeObject } from "./common/objects.mts";
import { Task, TaskId, writeTask } from "./common/tasks.mts";
import {
    Schedule,
    ScheduleV0,
} from "./common/schedule.mts";
import { Filter, initFilter } from "./common/filters.mts";

export class ShuttleContext {
    constructor(
        private _ctxData: ExtContextData,
        private _ctxHierarchy: ExtContextHierarchy,
        private _ctxFilters: ExtContextFilters,
    ) {

    }

    public getEvent(id: EventId): Event {
        return this._ctxData.events[id];
    }

    public getTask(id: TaskId): Task {
        return this._ctxData.tasks[id];
    }

    public getObject(id: ObjectId): Object {
        return this._ctxData.objects[id];
    }

    // TODO: this stateful interface is not great for notebook work
    public pushFilter(filter: Filter<any>) {
        this._ctxFilters.chain.push(initFilter(this._ctxData, filter));
    }

    public applyFiltersToEvent(event: Event): boolean {
        for (const filter of this._ctxFilters.chain) {
            if (filter.enabled && !filter.check(filter.data, event)) {
                return false;
            }
        }
        return true;
    }

    public applyFilters(): Event[] {
        return this._ctxData.events
            .filter((event) => this.applyFiltersToEvent(event));
    }

    public saveAnnotatedSchedule(): string {
        // re-number kept events
        let keptId = 0;
        let eventReindex = [];
        let keptEvents = [];
        for (let eventId = 0; eventId < this._ctxData.events.length; eventId++) {
            eventReindex[eventId] = keptId;
            if (this.applyFiltersToEvent(this._ctxData.events[eventId])) {
                keptEvents.push(this._ctxData.events[eventId]);
                keptId++;
            }
        }

        // re-number tasks and objects
        const schedule: ScheduleV0 = {
            version: 0,
            files: this._ctxData.source.files,
            functions: this._ctxData.source.functions,
            objects: this._ctxData.objects
                .map((object) => {
                    const ret = writeObject(object);
                    ret.created_at = eventReindex[ret.created_at];
                    return ret;
                }),
            tasks: this._ctxData.tasks
                .map((task) => {
                    const ret = writeTask(task);
                    ret.first_step = eventReindex[ret.first_step];
                    ret.last_step = eventReindex[ret.last_step];
                    return ret;
                }),
            events: keptEvents
                .map((event) => writeEvent(event)),
        }
        return JSON.stringify(schedule);
    }
}

export function loadAnnotatedSchedule(dataRaw: Uint8Array | string): ShuttleContext | null {
    const data: string = dataRaw instanceof Uint8Array
        ? new TextDecoder("utf-8").decode(dataRaw)
        : dataRaw;
    const schedule: Schedule = JSON.parse(data);
    const ctxData = createContextData(schedule);
    const ctxHierarchy = createContextHierarchy(ctxData);
    const ctxFilters = createContextFilters(ctxData);
    return new ShuttleContext(ctxData, ctxHierarchy, ctxFilters);
}
