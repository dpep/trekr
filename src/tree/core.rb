# Ruby's core library, as far as navigation cares.
#
# Not a runtime, not a type signature — just enough real Ruby that our own
# extractor can read it and the tree can answer "where does `puts` come from".
# It is deliberately ordinary source rather than RBS: no new parser, no new
# dependency, no second idea of what a method is, and any contributor can
# extend it by writing the method they were looking for (DEC-015).
#
# Bodies are empty on purpose. Ancestry is the load-bearing part — it is what
# gives every class an Object/Kernel/BasicObject tail — and the method lists
# cover what real code actually calls, not what exists.

class BasicObject
  def initialize; end
  def ==(other); end
  def !; end
  def !=(other); end
  def equal?(other); end
  def __send__(name, *args, &block); end
  def __id__; end
  def instance_eval(*args, &block); end
  def instance_exec(*args, &block); end
  def method_missing(name, *args, &block); end
  def singleton_method_added(name); end
end

module Kernel
  def puts(*args); end
  def print(*args); end
  def p(*args); end
  def pp(*args); end
  def raise(*args); end
  def fail(*args); end
  def require(name); end
  def require_relative(name); end
  def load(name, wrap = false); end
  def loop(&block); end
  def block_given?; end
  def format(fmt, *args); end
  def sprintf(fmt, *args); end
  def printf(*args); end
  def rand(max = nil); end
  def srand(number = nil); end
  def sleep(duration = nil); end
  def catch(tag = nil, &block); end
  def throw(tag, value = nil); end
  def lambda(&block); end
  def proc(&block); end
  def gets(*args); end
  def exit(status = true); end
  def exit!(status = false); end
  def abort(message = nil); end
  def at_exit(&block); end
  def caller(*args); end
  def caller_locations(*args); end
  def binding; end
  def freeze; end
  def frozen?; end
  def dup; end
  def clone(freeze: nil); end
  def itself; end
  def tap(&block); end
  def then(&block); end
  def yield_self(&block); end
  def object_id; end
  def hash; end
  def inspect; end
  def to_s; end
  def to_enum(*args, &block); end
  def enum_for(*args, &block); end
  def instance_variable_get(name); end
  def instance_variable_set(name, value); end
  def instance_variable_defined?(name); end
  def instance_variables; end
  def instance_of?(klass); end
  def is_a?(klass); end
  def kind_of?(klass); end
  def nil?; end
  def respond_to?(name, include_all = false); end
  def send(name, *args, &block); end
  def public_send(name, *args, &block); end
  def method(name); end
  def methods; end
  def public_methods(all = true); end
  def private_methods(all = true); end
  def singleton_class; end
  def define_singleton_method(name, *args, &block); end
  def extend(*modules); end
  def display(port = nil); end
  def warn(*messages); end
  def system(*args); end
  def spawn(*args); end
  def open(*args, &block); end
  def eql?(other); end
  def instance_variable_names; end
  def Integer(value, base = nil); end
  def Float(value); end
  def String(value); end
  def Array(value); end
  def Hash(value); end
  def Rational(*args); end
  def Complex(*args); end
end

class Object < BasicObject
  include Kernel

  def class; end
  def <=>(other); end
  def ===(other); end
  def =~(other); end
  def !~(other); end
end

class Module < Object
  def include(*modules); end
  def prepend(*modules); end
  def extend_object(object); end
  def included(base); end
  def extended(base); end
  def prepended(base); end
  def inherited(subclass); end
  def attr_reader(*names); end
  def attr_writer(*names); end
  def attr_accessor(*names); end
  def attr(*names); end
  def define_method(name, *args, &block); end
  def alias_method(new_name, old_name); end
  def remove_method(*names); end
  def undef_method(*names); end
  def private(*names); end
  def public(*names); end
  def protected(*names); end
  def module_function(*names); end
  def private_constant(*names); end
  def public_constant(*names); end
  def private_class_method(*names); end
  def public_class_method(*names); end
  def const_get(name, inherit = true); end
  def const_set(name, value); end
  def const_defined?(name, inherit = true); end
  def const_missing(name); end
  def constants(inherit = true); end
  def name; end
  def ancestors; end
  def included_modules; end
  def include?(mod); end
  def instance_methods(include_super = true); end
  def instance_method(name); end
  def public_instance_methods(include_super = true); end
  def private_instance_methods(include_super = true); end
  def method_defined?(name, inherit = true); end
  def private_method_defined?(name, inherit = true); end
  def instance_variable_get(name); end
  def module_eval(*args, &block); end
  def class_eval(*args, &block); end
  def module_exec(*args, &block); end
  def class_exec(*args, &block); end
  def define_singleton_method(name, *args, &block); end
  def <(other); end
  def <=(other); end
  def >(other); end
  def >=(other); end
end

class Class < Module
  def new(*args, &block); end
  def allocate; end
  def superclass; end
end

module Comparable
  def <(other); end
  def <=(other); end
  def >(other); end
  def >=(other); end
  def ==(other); end
  def between?(low, high); end
  def clamp(*args); end
end

module Enumerable
  # Abstract in core, but every includer defines it and navigation asks about
  # it constantly, so it is worth naming.
  def each(&block); end
  def each_entry(&block); end
  def map(&block); end
  def collect(&block); end
  def flat_map(&block); end
  def collect_concat(&block); end
  def select(&block); end
  def filter(&block); end
  def filter_map(&block); end
  def reject(&block); end
  def find(&block); end
  def detect(&block); end
  def find_all(&block); end
  def find_index(*args, &block); end
  def reduce(*args, &block); end
  def inject(*args, &block); end
  def each_with_index(*args, &block); end
  def each_with_object(memo, &block); end
  def each_slice(n, &block); end
  def each_cons(n, &block); end
  def sort(&block); end
  def sort_by(&block); end
  def min(*args, &block); end
  def max(*args, &block); end
  def min_by(*args, &block); end
  def max_by(*args, &block); end
  def minmax(&block); end
  def sum(init = 0, &block); end
  def count(*args, &block); end
  def group_by(&block); end
  def partition(&block); end
  def chunk_while(&block); end
  def slice_when(&block); end
  def tally; end
  def uniq(&block); end
  def zip(*others, &block); end
  def take(n); end
  def take_while(&block); end
  def drop(n); end
  def drop_while(&block); end
  def first(*args); end
  def include?(value); end
  def member?(value); end
  def to_a(*args); end
  def entries(*args); end
  def to_h(&block); end
  def to_set(*args); end
  def lazy; end
  def any?(*args, &block); end
  def all?(*args, &block); end
  def none?(*args, &block); end
  def one?(*args, &block); end
  def each_entry(&block); end
  def reverse_each(&block); end
  def cycle(n = nil, &block); end
  def with_index(offset = 0, &block); end
end

class NilClass < Object
  def to_a; end
  def to_s; end
  def to_h; end
  def nil?; end
  def &(other); end
  def |(other); end
end

class TrueClass < Object; end
class FalseClass < Object; end

class Symbol < Object
  include Comparable
  def to_proc; end
  def to_sym; end
  def to_s; end
  def name; end
  def length; end
  def upcase; end
  def downcase; end
  def start_with?(*prefixes); end
  def end_with?(*suffixes); end
  def [](*args); end
end

class Numeric < Object
  include Comparable
  def +(other); end
  def -(other); end
  def *(other); end
  def /(other); end
  def %(other); end
  def **(other); end
  def abs; end
  def round(*args); end
  def floor(*args); end
  def ceil(*args); end
  def to_i; end
  def to_int; end
  def to_f; end
  def to_r; end
  def zero?; end
  def positive?; end
  def negative?; end
  def nonzero?; end
  def coerce(other); end
  def divmod(other); end
  def clamp(*args); end
  def step(*args, &block); end
end

class Integer < Numeric
  def times(&block); end
  def upto(limit, &block); end
  def downto(limit, &block); end
  def succ; end
  def next; end
  def pred; end
  def even?; end
  def odd?; end
  def gcd(other); end
  def lcm(other); end
  def digits(base = 10); end
  def chr; end
  def ord; end
  def to_s(base = 10); end
  def fdiv(other); end
  def pow(*args); end
  def bit_length; end
end

class Float < Numeric
  def nan?; end
  def infinite?; end
  def finite?; end
  def truncate(*args); end
end

class Rational < Numeric; end
class Complex < Numeric; end

class String < Object
  include Comparable

  def +(other); end
  def *(count); end
  def %(args); end
  def <<(other); end
  def =~(other); end
  def [](*args); end
  def []=(*args); end
  def length; end
  def size; end
  def bytesize; end
  def empty?; end
  def to_s; end
  def to_str; end
  def to_sym; end
  def to_i(base = 10); end
  def to_f; end
  def to_r; end
  def to_c; end
  def upcase(*args); end
  def downcase(*args); end
  def capitalize(*args); end
  def swapcase(*args); end
  def strip; end
  def lstrip; end
  def rstrip; end
  def chomp(*args); end
  def chop; end
  def chars; end
  def bytes; end
  def lines(*args); end
  def each_char(&block); end
  def each_line(*args, &block); end
  def split(*args, &block); end
  def join(*args); end
  def sub(*args, &block); end
  def gsub(*args, &block); end
  def sub!(*args, &block); end
  def gsub!(*args, &block); end
  def tr(from, to); end
  def delete(*args); end
  def squeeze(*args); end
  def replace(other); end
  def insert(index, other); end
  def concat(*others); end
  def prepend(*others); end
  def start_with?(*prefixes); end
  def end_with?(*suffixes); end
  def include?(other); end
  def index(*args); end
  def rindex(*args); end
  def match(*args, &block); end
  def match?(*args); end
  def scan(pattern, &block); end
  def slice(*args); end
  def slice!(*args); end
  def center(width, pad = " "); end
  def ljust(width, pad = " "); end
  def rjust(width, pad = " "); end
  def reverse; end
  def freeze; end
  def frozen?; end
  def dup; end
  def hash; end
  def inspect; end
  def unpack(format); end
  def unpack1(format); end
  def encode(*args); end
  def force_encoding(encoding); end
  def encoding; end
  def valid_encoding?; end
  def unicode_normalize(*args); end
  def succ; end
  def next; end
  def ord; end
  def count(*args); end
  def format(*args); end
end

class Array < Object
  include Enumerable

  def [](*args); end
  def []=(*args); end
  def <<(value); end
  def +(other); end
  def -(other); end
  def *(other); end
  def &(other); end
  def |(other); end
  def length; end
  def size; end
  def empty?; end
  def push(*values); end
  def append(*values); end
  def pop(*args); end
  def shift(*args); end
  def unshift(*values); end
  def prepend(*values); end
  def insert(index, *values); end
  def delete(value, &block); end
  def delete_at(index); end
  def delete_if(&block); end
  def clear; end
  def concat(*others); end
  def compact; end
  def compact!; end
  def flatten(depth = nil); end
  def flatten!(depth = nil); end
  def uniq(&block); end
  def uniq!(&block); end
  def reverse; end
  def reverse!; end
  def rotate(count = 1); end
  def sort!(&block); end
  def sort_by!(&block); end
  def select!(&block); end
  def reject!(&block); end
  def map!(&block); end
  def collect!(&block); end
  def each(&block); end
  def each_index(&block); end
  def first(*args); end
  def last(*args); end
  def sample(*args); end
  def shuffle(*args); end
  def slice(*args); end
  def slice!(*args); end
  def fill(*args, &block); end
  def dig(*keys); end
  def values_at(*indexes); end
  def assoc(key); end
  def rassoc(value); end
  def index(*args, &block); end
  def rindex(*args, &block); end
  def join(separator = nil); end
  def pack(format); end
  def product(*others, &block); end
  def combination(n, &block); end
  def permutation(*args, &block); end
  def transpose; end
  def to_a; end
  def to_ary; end
  def to_h(&block); end
  def freeze; end
  def frozen?; end
  def hash; end
  def replace(other); end
  def bsearch(&block); end
end

class Hash < Object
  include Enumerable

  def [](key); end
  def []=(key, value); end
  def fetch(*args, &block); end
  def store(key, value); end
  def dig(*keys); end
  def delete(key, &block); end
  def delete_if(&block); end
  def keys; end
  def values; end
  def values_at(*keys); end
  def fetch_values(*keys, &block); end
  def key?(key); end
  def has_key?(key); end
  def include?(key); end
  def member?(key); end
  def value?(value); end
  def has_value?(value); end
  def key(value); end
  def length; end
  def size; end
  def empty?; end
  def each(&block); end
  def each_pair(&block); end
  def each_key(&block); end
  def each_value(&block); end
  def merge(*others, &block); end
  def merge!(*others, &block); end
  def update(*others, &block); end
  def transform_keys(*args, &block); end
  def transform_values(&block); end
  def transform_keys!(*args, &block); end
  def transform_values!(&block); end
  def select!(&block); end
  def reject!(&block); end
  def keep_if(&block); end
  def filter_map(&block); end
  def slice(*keys); end
  def except(*keys); end
  def compact; end
  def compact!; end
  def invert; end
  def to_h(&block); end
  def to_a; end
  def default; end
  def default=(value); end
  def default_proc; end
  def clear; end
  def freeze; end
  def frozen?; end
  def replace(other); end
  def any?(*args, &block); end
  def sum(init = 0, &block); end
  def group_by(&block); end
end

class Range < Object
  include Enumerable
  def begin; end
  def end; end
  def first(*args); end
  def last(*args); end
  def min(*args, &block); end
  def max(*args, &block); end
  def size; end
  def count(*args, &block); end
  def step(n = 1, &block); end
  def cover?(value); end
  def include?(value); end
  def each(&block); end
  def to_a; end
  def exclude_end?; end
end

class Struct < Object
  include Enumerable
  def self.new(*args, &block); end
  def members; end
  def to_a; end
  def to_h(&block); end
  def [](key); end
  def []=(key, value); end
  def each(&block); end
  def dig(*keys); end
  def deconstruct; end
  def deconstruct_keys(keys); end
end

class Data < Object
  def self.define(*names, &block); end
  def with(**kwargs); end
  def to_h(&block); end
  def members; end
  def deconstruct; end
  def deconstruct_keys(keys); end
end

class Set < Object
  include Enumerable
  def add(value); end
  def <<(value); end
  def add?(value); end
  def delete(value); end
  def include?(value); end
  def member?(value); end
  def size; end
  def length; end
  def empty?; end
  def each(&block); end
  def to_a; end
  def merge(*others); end
  def subset?(other); end
  def superset?(other); end
  def |(other); end
  def &(other); end
  def -(other); end
end

class Enumerator < Object
  include Enumerable
  def next; end
  def peek; end
  def rewind; end
  def size; end
  def with_index(offset = 0, &block); end
  def with_object(memo, &block); end
  def each(&block); end
end

class Proc < Object
  def call(*args, &block); end
  def ===(*args); end
  def [](*args); end
  def yield(*args); end
  def arity; end
  def lambda?; end
  def curry(arity = nil); end
  def to_proc; end
  def parameters; end
end

class Method < Object
  def call(*args, &block); end
  def to_proc; end
  def arity; end
  def name; end
  def owner; end
  def receiver; end
  def parameters; end
  def source_location; end
  def unbind; end
end

class UnboundMethod < Object
  def bind(receiver); end
  def name; end
  def owner; end
  def arity; end
  def source_location; end
end

class Binding < Object
  def local_variable_get(name); end
  def local_variable_set(name, value); end
  def local_variables; end
  def receiver; end
  def eval(*args); end
end

class Regexp < Object
  def match(*args, &block); end
  def match?(*args); end
  def =~(other); end
  def ===(other); end
  def source; end
  def options; end
  def names; end
  def self.escape(string); end
  def self.union(*patterns); end
  def self.last_match(*args); end
end

class MatchData < Object
  def [](*args); end
  def captures; end
  def named_captures; end
  def names; end
  def pre_match; end
  def post_match; end
  def to_a; end
  def begin(n); end
  def end(n); end
end

class Exception < Object
  def message; end
  def to_s; end
  def full_message(*args); end
  def backtrace; end
  def backtrace_locations; end
  def cause; end
  def exception(*args); end
  def self.exception(*args); end
end

class ScriptError < Exception; end
class LoadError < ScriptError; end
class NotImplementedError < ScriptError; end
class SyntaxError < ScriptError; end
class NoMemoryError < Exception; end
class SecurityError < Exception; end
class SystemExit < Exception; end
class SignalException < Exception; end
class Interrupt < SignalException; end
class SystemStackError < Exception; end

class StandardError < Exception; end
class RuntimeError < StandardError; end
class FrozenError < RuntimeError; end
class ArgumentError < StandardError; end
class TypeError < StandardError; end
class NameError < StandardError
  def name; end
  def receiver; end
end
class NoMethodError < NameError
  def args; end
end
class IndexError < StandardError; end
class KeyError < IndexError
  def key; end
  def receiver; end
end
class StopIteration < IndexError; end
class RangeError < StandardError; end
class FloatDomainError < RangeError; end
class ZeroDivisionError < StandardError; end
class IOError < StandardError; end
class EOFError < IOError; end
class LocalJumpError < StandardError; end
class RegexpError < StandardError; end
class ThreadError < StandardError; end
class FiberError < StandardError; end
class EncodingError < StandardError; end
class NoMatchingPatternError < StandardError; end
class NoMatchingPatternKeyError < NoMatchingPatternError; end
class UncaughtThrowError < ArgumentError; end
class ClosedQueueError < StopIteration; end

module Errno
  class ENOENT < StandardError; end
  class EACCES < StandardError; end
  class EEXIST < StandardError; end
  class EPIPE < StandardError; end
  class ECONNREFUSED < StandardError; end
  class ETIMEDOUT < StandardError; end
  class EISDIR < StandardError; end
  class ENOTDIR < StandardError; end
end

class IO < Object
  include Enumerable
  def read(*args); end
  def write(*args); end
  def puts(*args); end
  def print(*args); end
  def printf(*args); end
  def gets(*args); end
  def each_line(*args, &block); end
  def readlines(*args); end
  def readline(*args); end
  def close; end
  def closed?; end
  def flush; end
  def sync; end
  def sync=(value); end
  def fileno; end
  def eof?; end
  def rewind; end
  def seek(amount, whence = nil); end
  def pos; end
end

class File < IO
  def self.read(*args); end
  def self.write(*args); end
  def self.open(*args, &block); end
  def self.exist?(path); end
  def self.exists?(path); end
  def self.file?(path); end
  def self.directory?(path); end
  def self.readable?(path); end
  def self.writable?(path); end
  def self.executable?(path); end
  def self.size(path); end
  def self.size?(path); end
  def self.zero?(path); end
  def self.delete(*paths); end
  def self.unlink(*paths); end
  def self.rename(from, to); end
  def self.join(*parts); end
  def self.expand_path(path, base = nil); end
  def self.absolute_path(path, base = nil); end
  def self.basename(path, suffix = nil); end
  def self.dirname(path, level = 1); end
  def self.extname(path); end
  def self.split(path); end
  def self.readlines(*args); end
  def self.foreach(*args, &block); end
  def self.binread(*args); end
  def self.binwrite(*args); end
  def self.mtime(path); end
  def self.ctime(path); end
  def self.atime(path); end
  def self.stat(path); end
  def self.symlink?(path); end
  def self.realpath(path, base = nil); end
  def path; end
end

class Dir < Object
  include Enumerable
  def self.glob(*args, &block); end
  def self.[](*args); end
  def self.entries(*args); end
  def self.children(*args); end
  def self.each_child(*args, &block); end
  def self.mkdir(path, mode = nil); end
  def self.rmdir(path); end
  def self.exist?(path); end
  def self.pwd; end
  def self.chdir(path = nil, &block); end
  def self.home(user = nil); end
  def self.tmpdir; end
end

class Time < Object
  include Comparable
  def self.now(*args); end
  def self.at(*args); end
  def self.parse(*args); end
  def self.new(*args); end
  def year; end
  def month; end
  def day; end
  def hour; end
  def min; end
  def sec; end
  def usec; end
  def nsec; end
  def wday; end
  def yday; end
  def zone; end
  def to_i; end
  def to_f; end
  def to_r; end
  def to_s; end
  def utc; end
  def utc?; end
  def localtime(*args); end
  def getlocal(*args); end
  def strftime(format); end
  def +(other); end
  def -(other); end
end

class Random < Object
  def self.rand(max = nil); end
  def self.new_seed; end
  def self.srand(number = nil); end
  def rand(max = nil); end
  def seed; end
  def bytes(count); end
end

class Thread < Object
  def self.new(*args, &block); end
  def self.current; end
  def self.main; end
  def self.list; end
  def join(limit = nil); end
  def value; end
  def alive?; end
  def kill; end
  def [](key); end
  def []=(key, value); end
  def name; end
  def name=(value); end
end

class Mutex < Object
  def lock; end
  def unlock; end
  def locked?; end
  def synchronize(&block); end
  def try_lock; end
end

class Queue < Object
  def push(value); end
  def <<(value); end
  def pop(non_block = false); end
  def size; end
  def length; end
  def empty?; end
  def close; end
  def closed?; end
end

class SizedQueue < Queue; end
class ConditionVariable < Object
  def wait(mutex, timeout = nil); end
  def signal; end
  def broadcast; end
end

class Fiber < Object
  def self.yield(*args); end
  def self.new(&block); end
  def resume(*args); end
  def alive?; end
end

class Ractor < Object; end

module Math
  def self.sqrt(value); end
  def self.cbrt(value); end
  def self.log(*args); end
  def self.log2(value); end
  def self.log10(value); end
  def self.exp(value); end
  def self.sin(value); end
  def self.cos(value); end
  def self.tan(value); end
  def self.atan(value); end
  def self.atan2(y, x); end
  def self.hypot(x, y); end
  def self.pow(x, y); end
end

module ObjectSpace
  def self.each_object(*args, &block); end
  def self.garbage_collect(*args); end
  def self.define_finalizer(object, proc = nil); end
  def self.count_objects(*args); end
end

module GC
  def self.start(*args); end
  def self.stat(*args); end
  def self.disable; end
  def self.enable; end
  def self.compact; end
end

module Marshal
  def self.dump(*args); end
  def self.load(*args); end
end

module Process
  def self.pid; end
  def self.ppid; end
  def self.exit(status = true); end
  def self.exit!(status = false); end
  def self.fork(&block); end
  def self.wait(*args); end
  def self.spawn(*args); end
  def self.kill(signal, *pids); end
  def self.clock_gettime(clock, unit = nil); end
end

module Signal
  def self.trap(signal, command = nil, &block); end
  def self.list; end
end

module Warning
  def self.warn(message, category: nil); end
end

class Encoding < Object
  def self.default_external; end
  def self.default_internal; end
  def name; end
end

class Enumerator
  class Lazy < Enumerator; end
  class Yielder < Object
    def <<(value); end
    def yield(*args); end
  end
end

module FileUtils
  def self.mkdir_p(*args); end
  def self.rm_rf(*args); end
  def self.rm_f(*args); end
  def self.cp(*args); end
  def self.cp_r(*args); end
  def self.mv(*args); end
  def self.touch(*args); end
  def self.ln_s(*args); end
end

ENV = nil
ARGV = nil
RUBY_VERSION = nil
RUBY_PLATFORM = nil
RUBY_ENGINE = nil
