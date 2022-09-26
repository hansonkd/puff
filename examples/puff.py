from greenlet import greenlet
import dataclasses
import contextvars
from threading import Thread
from typing import Any
import queue

parent_thread = contextvars.ContextVar("parent_thread")
greenlet_context = contextvars.ContextVar("greenlet_context")


@dataclasses.dataclass(frozen=True, slots=True)
class Task:
    args: list
    kwargs: dict
    ret_func: Any
    task_function: Any
    context: Any

    def process(self):
        new_greenlet = greenlet(self.task_function)
        new_greenlet.switch(self.args, self.kwargs, self.ret_func, self.context)


@dataclasses.dataclass(frozen=True, slots=True)
class Result:
    greenlet: Any

    def process(self):
        self.greenlet.switch()


class MainThread(Thread):
    def __init__(self, event_queue, on_thread_start=None):
        self.event_queue = event_queue
        # self.task_spawner = task_spawner
        self.on_thread_start = on_thread_start
        self.main_greenlet = None
        self.event_loop_processor = None
        self.read_from_queue_processor = None
        super().__init__()

    def run(self):
        if self.on_thread_start is not None:
            self.on_thread_start()
        self.main_greenlet = greenlet.getcurrent()
        self.event_loop_processor = greenlet(self.loop_commands)
        self.read_from_queue_processor = greenlet(self.read_from_queue)
        self.event_loop_processor.switch()
        while True:
            self.read_from_queue_processor.switch()

    def spawn(self, task_function, context, args, kwargs, ret_func):
        task_function_wrapped = self.generate_spawner(task_function)
        task = Task(args=args, kwargs=kwargs, ret_func=ret_func, task_function=task_function_wrapped, context=context)
        self.event_queue.put(task)

    def return_result(self, greenlet):
        result_event = Result(greenlet=greenlet)
        self.event_queue.put(result_event)

    def generate_spawner(self, func):
        def override_spawner(args, kwargs, ret_func, context):
            parent_thread.set(self)
            greenlet_context.set(context)
            try:
                val = func(*args, **kwargs)
                ret_func(val, None)
            except Exception as e:
                ret_func(None, e)

        return override_spawner

    def loop_commands(self):
        while True:
            event = self.read_next_event()
            event.process()

    def read_from_queue(self):
        while True:
            event = self.event_queue.get()
            if event is None:
                break
            self.event_loop_processor.switch(event)

    def read_next_event(self):
        res = self.main_greenlet.switch("blocking here")
        return res


def start_event_loop():
    q = queue.Queue()

    loop_thread = MainThread(q)
    loop_thread.start()

    return loop_thread


@dataclasses.dataclass
class Greenlet:
    thread: Any
    result: Any = None
    exception: Any = None
    finished: bool = False

    def join(self):
        while not self.finished:
            self.thread.event_loop_processor.switch()

        if self.exception:
            raise self.exception
        else:
            return self.result

    def set_result(self, result, exception):
        self.result = result
        self.exception = exception
        self.finished = True


def wrap_async(f, join=True):
    this_greenlet = greenlet.getcurrent()
    thread = parent_thread.get()
    context = greenlet_context.get()

    greenlet_obj = Greenlet(thread=thread)

    def return_result(r, e):
        greenlet_obj.set_result(r, e)
        thread.return_result(this_greenlet)

    f(context, return_result)
    if join:
        return greenlet_obj.join()
    else:
        return greenlet_obj


def spawn(f, *args, **kwargs):

    thread = parent_thread.get()
    context = greenlet_context.get()
    this_greenlet = greenlet.getcurrent()

    greenlet_obj = Greenlet(thread=thread)

    def return_result(val, e):
        greenlet_obj.set_result(val, e)
        this_greenlet.switch()

    def run_and_catch():
        try:
            res = f(*args, **kwargs)
            return res, None
        except Exception as e:
            return None, e

    thread.spawn(run_and_catch, context, [], {}, return_result)

    result, exception = thread.event_loop_processor.switch()
    if exception:
        raise exception
    else:
        return result


def join_all(greenlets):
    if not greenlets:
        return []
    thread = greenlets[0].thread
    while not all(g.finished for g in greenlets):
        thread.event_loop_processor.switch()
    return [g.result for g in greenlets]


def join_iter(greenlets):
    if not greenlets:
        return None

    thread = greenlets[0].thread
    left = set(greenlets)

    while left:
        thread.event_loop_processor.switch()
        remove = set()
        for x in left:
            if x.finished:
                yield x.result
                remove.add(x)
        left = left - remove


def get_redis():
    pass


def global_state():
    return greenlet_context.get().global_state()

