from greenlet import greenlet
import dataclasses
import contextvars
from threading import Thread
from typing import Any
import queue


class RustObjects(object):
    """A class that holds functions and objects from Rust."""
    global_state: Any
    global_redis_setter: Any


rust_objects = RustObjects()

#  A global context var which holds information about the current executing thread.
parent_thread = contextvars.ContextVar("parent_thread")


@dataclasses.dataclass(frozen=True, slots=True)
class Task:
    args: list
    kwargs: dict
    ret_func: Any
    task_function: Any

    def process(self):
        new_greenlet = greenlet(self.task_function)
        new_greenlet.switch(self.args, self.kwargs, self.ret_func)


@dataclasses.dataclass(frozen=True, slots=True)
class Result:
    greenlet: Any

    def process(self):
        self.greenlet.switch()


@dataclasses.dataclass(frozen=True, slots=True)
class Kill:
    thread: Any

    def process(self):
        self.thread.kill_now()


class MainThread(Thread):
    def __init__(self, event_queue, on_thread_start=None):
        self.event_queue = event_queue
        self.shutdown_started = False
        self.on_thread_start = on_thread_start
        self.main_greenlet = None
        self.event_loop_processor = None
        self.read_from_queue_processor = None
        self.greenlets = set()
        super().__init__()

    def run(self):
        if self.on_thread_start is not None:
            self.on_thread_start()
        self.main_greenlet = greenlet.getcurrent()
        self.event_loop_processor = greenlet(self.loop_commands)
        self.read_from_queue_processor = greenlet(self.read_from_queue)
        self.event_loop_processor.switch()
        while self.read_from_queue_processor.switch():
            pass

    def spawn(self, task_function, args, kwargs, ret_func):
        greenlet = self.new_greenlet()

        def wrapped_ret(val, e):
            self.complete_greenlet(greenlet)
            ret_func(val, e)

        self.spawn_local(task_function, args, kwargs, wrapped_ret)

    def spawn_local(self, task_function, args, kwargs, ret_func):
        task_function_wrapped = self.generate_spawner(task_function)
        task = Task(args=args, kwargs=kwargs, ret_func=ret_func, task_function=task_function_wrapped)
        self.event_queue.put(task)

    def new_greenlet(self):
        greenlet = Greenlet(thread=self)
        self.greenlets.add(greenlet)
        return greenlet

    def complete_greenlet(self, greenlet):
        self.greenlets.remove(greenlet)

    def return_result(self, greenlet):
        result_event = Result(greenlet=greenlet)
        self.event_queue.put(result_event)

    def generate_spawner(self, func):
        def override_spawner(args, kwargs, ret_func):
            parent_thread.set(self)
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
        while not self.has_shutdown():
            event = self.event_queue.get()
            self.event_loop_processor.switch(event)
        self.kill_now()

    def kill(self):
        self.event_queue.put(Kill(thread=self))

    def kill_now(self):
        self.main_greenlet.switch(False)

    def start_shutdown(self):
        self.shutdown_started = True

    def has_shutdown(self):
        return self.shutdown_started and not self.greenlets

    def read_next_event(self):
        res = self.main_greenlet.switch(True)
        return res


def start_event_loop(on_thread_start=None):
    q = queue.Queue()

    loop_thread = MainThread(q, on_thread_start=on_thread_start)
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
        self.thread.complete_greenlet(self)

    def __hash__(self):
        return id(self)


def wrap_async(f, join=True):
    this_greenlet = greenlet.getcurrent()
    thread = parent_thread.get()

    greenlet_obj = thread.new_greenlet()

    def return_result(r, e):
        greenlet_obj.set_result(r, e)
        thread.return_result(this_greenlet)

    f(return_result)
    if join:
        return greenlet_obj.join()
    else:
        return greenlet_obj


def spawn(f, *args, **kwargs):
    thread = parent_thread.get()
    this_greenlet = greenlet.getcurrent()
    greenlet_obj = thread.new_greenlet()

    def return_result(val, e):
        greenlet_obj.set_result(val, e)
        thread.return_result(this_greenlet)

    thread.spawn_local(f, args, kwargs, return_result, greenlet_obj=greenlet_obj)

    return greenlet_obj


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
    pending = set(greenlets)

    while pending:
        thread.event_loop_processor.switch()
        remove = set()
        for x in pending:
            if x.finished:
                yield x.result
                remove.add(x)
        pending = pending - remove


def global_redis():
    return rust_objects.global_redis_getter()


def global_state():
    return rust_objects.global_state


def spawn_blocking(f, *args, **kwargs):
    thread = parent_thread.get()
    child_thread = start_event_loop(on_thread_start=thread.on_thread_start)
    this_greenlet = greenlet.getcurrent()
    greenlet_obj = thread.new_greenlet()

    def return_result(val, e):
        greenlet_obj.set_result(val, e)
        thread.return_result(this_greenlet)
        child_thread.start_shutdown()

    child_thread.spawn(f, args, kwargs, return_result)

    return greenlet_obj


def spawn_blocking_from_rust(on_thread_start, f, return_result, args, kwargs):
    child_thread = start_event_loop(on_thread_start=on_thread_start)

    def wrap_return_result(val, e):
        return_result(val, e)
        child_thread.start_shutdown()

    child_thread.spawn(f, args, kwargs, wrap_return_result)
