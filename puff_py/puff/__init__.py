"""
Puff v2 Python Runtime

Under free-threaded Python (3.13t+), real OS threads replace greenlets.
Each Puff task runs on its own thread. I/O calls cross into Rust via PyO3,
dispatch to Tokio, and the thread blocks on a channel until the result returns.

Under GIL Python, we fall back to the original greenlet-based event loop
for backward compatibility.
"""
import queue
import sys
import threading
import traceback
import contextvars
import dataclasses
import concurrent.futures
from importlib import import_module
from pkgutil import extend_path
from typing import Any, Union
from threading import Thread, local
import functools
from puff.tools import tool

__path__ = extend_path(__path__, __name__)

thread_local = local()
ASYNCIO = object()
THREADED = object()
GREENLET = object()

# Detect free-threaded Python
FREE_THREADED = hasattr(sys, '_is_gil_enabled') and not sys._is_gil_enabled()

# Try to import greenlet — only needed under GIL Python
_greenlet = None
if not FREE_THREADED:
    try:
        from greenlet import greenlet as _greenlet_cls
        _greenlet = _greenlet_cls
    except ImportError:
        pass

global_state = None


def set_async_type(async_type):
    thread_local.async_type = async_type


def is_asyncio():
    return getattr(thread_local, "async_type", None) == ASYNCIO


def is_greenlet():
    return getattr(thread_local, "async_type", None) == GREENLET


def is_threaded():
    return getattr(thread_local, "async_type", None) == THREADED


def blocking(func):
    if rust_objects.is_puff:
        @functools.wraps(func)
        def wrapper_blocking(*args, **kwargs):
            if FREE_THREADED or is_threaded():
                # Under free-threaded Python, just run it — threads are real
                return func(*args, **kwargs)
            else:
                return spawn_blocking(func, *args, **kwargs).join()
        return wrapper_blocking
    else:
        return func


class RustObjects(object):
    """A class that holds functions and objects from Rust."""

    is_puff: bool = False
    asyncio_loop = None

    def global_redis_getter(self):
        return None

    def global_postgres_getter(self):
        return None

    def global_pubsub_getter(self):
        return None

    def global_gql_getter(self):
        return None

    def global_task_queue_getter(self):
        return None

    def global_http_client_getter(self):
        return None

    def dispatch_greenlet(self, ret, f):
        raise NotImplementedError

    def dispatch_asyncio(self, ret, f):
        raise NotImplementedError

    def dispatch_asyncio_coro(self, ret, f):
        raise NotImplementedError

    def read_file_bytes(self, rr: Any, fn: str) -> bytes:
        pass

    def sleep_ms(self, rr: Any, fn: int) -> bytes:
        pass


rust_objects = RustObjects()

parent_thread = contextvars.ContextVar("parent_thread")


# ─── Free-Threaded Mode: Thread-Based Execution ────────────────────

class ThreadTask:
    """A task that runs on a real thread under free-threaded Python."""
    def __init__(self):
        self._future = concurrent.futures.Future()

    @property
    def result(self):
        return self._future.result()

    @property
    def exception(self):
        return self._future.exception()

    @property
    def finished(self):
        return self._future.done()

    def join(self):
        try:
            return self._future.result()
        except Exception:
            raise

    def set_result(self, result, exception):
        if exception:
            self._future.set_exception(exception)
        else:
            self._future.set_result(result)

    def __hash__(self):
        return id(self)


# Global thread pool for free-threaded mode
_thread_pool = None


def _get_thread_pool():
    global _thread_pool
    if _thread_pool is None:
        import os
        _thread_pool = concurrent.futures.ThreadPoolExecutor(
            max_workers=int(os.environ.get("PUFF_THREAD_POOL_SIZE", os.cpu_count() * 4))
        )
    return _thread_pool


# ─── GIL Mode: Greenlet-Based Execution ────────────────────────────

if _greenlet is not None:
    @dataclasses.dataclass(frozen=True, slots=True)
    class Task:
        args: list
        kwargs: dict
        ret_func: Any
        task_function: Any

        def process(self):
            new_greenlet = _greenlet(self.task_function)
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

    @dataclasses.dataclass(frozen=True, slots=True)
    class StartShutdown:
        thread: Any

        def process(self):
            self.thread.do_shutdown()

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
            set_async_type(GREENLET)
            if self.on_thread_start is not None:
                self.on_thread_start()
            self.main_greenlet = _greenlet.getcurrent()
            self.event_loop_processor = _greenlet(self.loop_commands)
            self.read_from_queue_processor = _greenlet(self.read_from_queue)
            self.event_loop_processor.switch()
            while self.read_from_queue_processor.switch():
                pass

        def spawn(self, task_function, args, kwargs, ret_func):
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
            self.event_queue.put(Result(greenlet=greenlet))

        def generate_spawner(self, func):
            def override_spawner(args, kwargs, ret_func):
                parent_thread.set(self)
                try:
                    val = func(*args, **kwargs)
                    ret_func(val, None)
                except Exception as e:
                    exc_type, exc_value, exc_traceback = sys.exc_info()
                    traceback.print_tb(exc_traceback, file=sys.stderr)
                    traceback.print_exception(exc_value, file=sys.stderr)
                    ret_func(None, e)
            return override_spawner

        def loop_commands(self):
            while True:
                event = self.read_next_event()
                event.process()

        def read_from_queue(self):
            parent_thread.set(self)
            while not self.has_shutdown():
                event = self.event_queue.get()
                self.event_loop_processor.switch(event)
            self.kill_now()

        def kill(self):
            self.event_queue.put(Kill(thread=self))

        def kill_now(self):
            self.main_greenlet.switch(False)

        def start_shutdown(self):
            self.event_queue.put(StartShutdown(thread=self))

        def do_shutdown(self):
            self.shutdown_started = True

        def has_shutdown(self):
            return self.shutdown_started and not self.greenlets

        def read_next_event(self):
            return self.main_greenlet.switch(True)

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
            return self.result

        def set_result(self, result, exception):
            self.result = result
            self.exception = exception
            self.finished = True
            self.thread.complete_greenlet(self)

        def __hash__(self):
            return id(self)


def start_event_loop(q=None, on_thread_start=None):
    if FREE_THREADED:
        # No event loop needed — threads are real
        return None
    if _greenlet is None:
        raise ImportError("greenlet is required for GIL-mode Python. Install: pip install greenlet")
    if q is None:
        q = queue.Queue()
    loop_thread = MainThread(q, on_thread_start=on_thread_start)
    loop_thread.start()
    return loop_thread


# ─── Unified API ───────────────────────────────────────────────────

Bytelike = Union[str, bytes]


def wrap_async_asyncio(f, wrap_return=None):
    loop = rust_objects.asyncio_loop
    if loop is None:
        raise RuntimeError("AsyncIO not configured in Puff RuntimeConfig")
    future = loop.create_future()

    def wrapped_ret(val, e):
        if e is None and wrap_return is not None:
            val = wrap_return(val)
        if e is None:
            loop.call_soon_threadsafe(future.set_result, val)
        else:
            loop.call_soon_threadsafe(future.set_exception, e)

    f(wrapped_ret)
    return future


def wrap_async_threaded(f, wrap_return=None):
    """Under free-threaded Python, call the Rust async function and block."""
    result_holder = [None, None]
    event = threading.Event()

    def return_result(val, e):
        if e is None and wrap_return is not None:
            val = wrap_return(val)
        result_holder[0] = val
        result_holder[1] = e
        event.set()

    f(return_result)
    event.wait()

    if result_holder[1] is not None:
        raise result_holder[1]
    return result_holder[0]


def wrap_async_greenlet(f, join=True, wrap_return=None):
    if _greenlet is None:
        raise ImportError("greenlet required for GIL-mode")
    this_greenlet = _greenlet.getcurrent()
    thread = parent_thread.get()
    greenlet_obj = thread.new_greenlet()

    def return_result(r, e):
        if e is None and wrap_return is not None:
            r = wrap_return(r)
        greenlet_obj.set_result(r, e)
        thread.return_result(this_greenlet)

    f(return_result)
    if join:
        return greenlet_obj.join()
    return greenlet_obj


def wrap_async(f, join=True, wrap_return=None):
    if is_asyncio():
        return wrap_async_asyncio(f, wrap_return=wrap_return)
    elif FREE_THREADED or is_threaded():
        return wrap_async_threaded(f, wrap_return=wrap_return)
    elif is_greenlet():
        return wrap_async_greenlet(f, join=join, wrap_return=wrap_return)
    else:
        # Default: try threaded mode (works for simple scripts)
        return wrap_async_threaded(f, wrap_return=wrap_return)


def spawn(f, *args, **kwargs):
    if is_asyncio():
        return spawn_from_asyncio(f, *args, **kwargs)
    elif FREE_THREADED or is_threaded():
        return spawn_threaded(f, *args, **kwargs)
    elif is_greenlet():
        return spawn_from_greenlet(f, *args, **kwargs)
    else:
        return spawn_threaded(f, *args, **kwargs)


def spawn_from_asyncio(f, *args, **kwargs):
    raise RuntimeError("Don't use spawn from AsyncIO, instead use asgiref.sync_to_async")


def spawn_threaded(f, *args, **kwargs):
    """Spawn a function on the thread pool. Returns a ThreadTask."""
    task = ThreadTask()
    pool = _get_thread_pool()
    def run():
        try:
            result = f(*args, **kwargs)
            task.set_result(result, None)
        except Exception as e:
            task.set_result(None, e)
    pool.submit(run)
    return task


def spawn_from_greenlet(f, *args, **kwargs):
    if _greenlet is None:
        return spawn_threaded(f, *args, **kwargs)
    thread = parent_thread.get()
    this_greenlet = _greenlet.getcurrent()
    greenlet_obj = thread.new_greenlet()

    def return_result(val, e):
        greenlet_obj.set_result(val, e)
        thread.return_result(this_greenlet)

    thread.spawn(f, args, kwargs, return_result)
    sleep_ms(0)
    return greenlet_obj


def join_all(tasks):
    """Wait for all tasks to finish."""
    if not tasks:
        return []
    if FREE_THREADED:
        return [t.join() for t in tasks]
    thread = tasks[0].thread
    while not all(g.finished for g in tasks):
        thread.event_loop_processor.switch()
    return [g.result for g in tasks]


def join_iter(tasks):
    """Wait for tasks, yielding results as they become available."""
    if not tasks:
        return
    if FREE_THREADED:
        for t in concurrent.futures.as_completed([t._future for t in tasks]):
            yield t.result()
        return
    thread = tasks[0].thread
    pending = set(tasks)
    while pending:
        thread.event_loop_processor.switch()
        remove = set()
        for x in pending:
            if x.finished:
                yield x.result
                remove.add(x)
        pending -= remove


def sleep_ms(time_to_sleep_ms: int):
    return wrap_async(lambda rr: rust_objects.sleep_ms(rr, time_to_sleep_ms), join=True)


def spawn_blocking(f, *args, **kwargs):
    if FREE_THREADED:
        return spawn_threaded(f, *args, **kwargs)
    if _greenlet is None:
        return spawn_threaded(f, *args, **kwargs)
    if not is_greenlet():
        raise RuntimeError("Blocking functions can only be spawned from a greenlet")
    thread = parent_thread.get()
    child_thread = start_event_loop(on_thread_start=thread.on_thread_start)
    this_greenlet = _greenlet.getcurrent()
    greenlet_obj = thread.new_greenlet()

    def return_result(val, e):
        greenlet_obj.set_result(val, e)
        thread.return_result(this_greenlet)
        child_thread.start_shutdown()

    child_thread.spawn(f, args, kwargs, return_result)
    return greenlet_obj


def spawn_blocking_from_rust(on_thread_start, f, args, kwargs, return_result):
    if FREE_THREADED:
        # Under free-threaded Python, just run on a thread
        pool = _get_thread_pool()
        def run():
            try:
                val = f(*args, **kwargs)
                return_result(val, None)
            except Exception as e:
                return_result(None, e)
        pool.submit(run)
        return
    child_thread = start_event_loop(on_thread_start=on_thread_start)

    def wrap_return_result(val, e):
        return_result(val, e)
        child_thread.start_shutdown()

    child_thread.spawn(f, args, kwargs, wrap_return_result)


def cached_import(module_path, class_name):
    if not (module := sys.modules.get(module_path)):
        module = import_module(module_path)
    return getattr(module, class_name)


def import_string(dotted_path):
    try:
        module_path, class_name = dotted_path.rsplit(".", 1)
    except ValueError as err:
        raise ImportError("%s doesn't look like a module path" % dotted_path) from err
    return cached_import(module_path, class_name)


def read_file_bytes(fn: str) -> bytes:
    return wrap_async(lambda rr: rust_objects.read_file_bytes(rr, fn), join=True)


context_id_var = contextvars.ContextVar("context_id")


def patch_asgi_ref_local():
    try:
        from asgiref import sync
    except ImportError:
        return None

    class ContextId:
        pass

    class SyncToAsync(sync.SyncToAsync):
        @staticmethod
        def get_current_task():
            if context_id := context_id_var.get(None):
                return context_id
            context_id = ContextId()
            context_id_var.set(context_id)
            return context_id

        async def __call__(self, *args, **kwargs):
            if is_asyncio():
                context = contextvars.copy_context()
                def wrapped():
                    child = functools.partial(self.func, *args, **kwargs)
                    return context.run(child)
                if FREE_THREADED:
                    return await wrap_async_asyncio(
                        lambda ret: _get_thread_pool().submit(lambda: _run_and_return(wrapped, ret))
                    )
                else:
                    return await wrap_async_asyncio(
                        lambda ret: rust_objects.dispatch_greenlet(ret, wrapped)
                    )
            else:
                return await super().__call__(*args, **kwargs)

    class AsyncToSync(sync.AsyncToSync):
        def __call__(self, *args, **kwargs):
            if is_greenlet() and not FREE_THREADED:
                if rust_objects.asyncio_loop is None:
                    raise RuntimeError("AsyncIO not configured in Puff RuntimeConfig")
                source_thread = threading.current_thread()
                context = [contextvars.copy_context()]
                async def await_f():
                    call_result = rust_objects.asyncio_loop.create_future()
                    await self.main_wrap(args, kwargs, call_result, source_thread, sys.exc_info(), context)
                    return await call_result
                return wrap_async_greenlet(
                    lambda ret: rust_objects.dispatch_asyncio(ret, await_f), join=True
                )
            else:
                return super().__call__(*args, **kwargs)

    sync.SyncToAsync = SyncToAsync
    sync.AsyncToSync = AsyncToSync
    sync.async_to_sync = AsyncToSync
    sync.sync_to_async = SyncToAsync


def _run_and_return(f, ret):
    try:
        val = f()
        ret(val, None)
    except Exception as e:
        ret(None, e)


def patch_django():
    try:
        from django.views import static
    except ImportError:
        return None
    from puff.contrib.django.static import serve
    static.serve = serve


def patch_psycopg2():
    try:
        import psycopg2
    except ImportError:
        return None
    from puff import postgres
    for s in dir(postgres):
        if not s.startswith("__"):
            setattr(psycopg2, s, getattr(postgres, s))


def patch_libs():
    patch_asgi_ref_local()
    patch_django()
    patch_psycopg2()
