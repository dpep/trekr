# Ruby's core library, as far as navigation cares.
#
# Not a runtime — just enough real Ruby that our own extractor can read it and
# the tree can answer "where does `puts` come from". It is deliberately
# ordinary source rather than RBS: no new parser, no new dependency, no second
# idea of what a method is, and any contributor can extend it by writing the
# method they were looking for (DEC-015).
#
# Bodies are empty on purpose. Ancestry is the load-bearing part — it is what
# gives every class an Object/Kernel/BasicObject tail — and the method lists
# cover what real code actually calls, not what exists. `initialize` is the
# exception: it is declared wherever Ruby defines its own, because `super` in
# an `initialize` lands on it, and a missing one sends it on to BasicObject.
#
# Parameters and `sig`s come from Ruby 3.4's RBS, through
# `script/core_sigs.rb`: add a `def`, rerun it. A `sig` is a return type a
# call chain is typed from (`x.gsub(a, b).downcase` is a String); several are
# overloads, told apart by the call's argument count and block (DEC-077).
#
# Each top-level class or module is served as its own file (`String.rb`), so a
# definition lands somewhere a person can read; code outside one goes to
# `Object.rb`. A class is declared once here — reopening one would split it.

class BasicObject
  def initialize
  end

  def ==(other)
  end

  def !
  end

  def !=(other)
  end

  def equal?(other)
  end

  def __send__(name, *args, **options, &block)
  end

  sig { returns(Integer) }
  def __id__
  end

  def instance_eval(code = nil, filename = nil, lineno = nil, &block)
  end

  def instance_exec(*args, **options, &block)
  end

  def method_missing(name, *args, &block)
  end

  def singleton_method_added(symbol)
  end
end

module Kernel
  def puts(*objects)
  end

  def print(*objects)
  end

  def p(object = nil, *objects)
  end

  def pp(*objs)
  end

  def raise(exception = nil, message = nil, backtrace = nil, cause: nil, **options)
  end

  def fail(exception = nil, message = nil, backtrace = nil, cause: nil, **options)
  end

  def require(path)
  end

  def require_relative(string)
  end

  def load(filename, wrap = false)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def loop(&block)
  end

  def block_given?
  end

  sig { returns(String) }
  def format(format, *args)
  end

  sig { returns(String) }
  def sprintf(format, *args)
  end

  def printf(io = nil, format_string = nil, *objects)
  end

  def rand(max = 0)
  end

  sig { returns(Integer) }
  def srand(number = nil)
  end

  def sleep(secs = nil)
  end

  def catch(tag = nil, &block)
  end

  def throw(tag, obj = nil)
  end

  sig { params(block: T.proc.void).returns(Proc) }
  def lambda(&block)
  end

  sig { params(block: T.proc.void).returns(Proc) }
  def proc(&block)
  end

  def gets(sep = nil, arg1 = nil)
  end

  def exit(status = true)
  end

  def exit!(status = false)
  end

  def abort(msg = nil)
  end

  sig { params(block: T.proc.void).returns(Proc) }
  def at_exit(&block)
  end

  def caller(start = 1, length = nil)
  end

  def caller_locations(start = 1, length = nil)
  end

  sig { returns(Binding) }
  def binding
  end

  def freeze
  end

  def frozen?
  end

  def dup
  end

  def clone(freeze: nil)
  end

  def itself
  end

  def tap(&block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def then(&block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def yield_self(&block)
  end

  sig { returns(Integer) }
  def object_id
  end

  sig { returns(Integer) }
  def hash
  end

  sig { returns(String) }
  def inspect
  end

  sig { returns(String) }
  def to_s
  end

  sig { returns(Enumerator) }
  def to_enum(method = nil, *args, **options, &block)
  end

  sig { returns(Enumerator) }
  def enum_for(method = nil, *args, **options, &block)
  end

  def instance_variable_get(symbol)
  end

  def instance_variable_set(symbol, obj)
  end

  def instance_variable_defined?(symbol)
  end

  sig { returns(Array) }
  def instance_variables
  end

  def instance_of?(module_or_class)
  end

  def is_a?(module_or_class)
  end

  def kind_of?(module_or_class)
  end

  def nil?
  end

  def respond_to?(symbol, include_all = false)
  end

  def send(name, *args, **options, &block)
  end

  def public_send(name, *args, **options, &block)
  end

  sig { returns(Method) }
  def method(sym)
  end

  sig { returns(Array) }
  def methods(regular = true)
  end

  sig { returns(Array) }
  def public_methods(all = true)
  end

  sig { returns(Array) }
  def private_methods(all = true)
  end

  def singleton_class
  end

  sig { returns(Symbol) }
  def define_singleton_method(symbol, method = nil, &block)
  end

  def extend(mod, *modules)
  end

  def display(port = $>)
  end

  def warn(*msgs, uplevel: nil, category: nil)
  end

  def system(env, command = nil, *args, unsetenv_others: nil, pgroup: nil, umask: nil, in: nil, out: nil, err: nil, close_others: nil, chdir: nil, exception: nil)
  end

  sig { returns(Integer) }
  def spawn(env, command = nil, *args, unsetenv_others: nil, pgroup: nil, umask: nil, in: nil, out: nil, err: nil, close_others: nil, chdir: nil)
  end

  def open(path, mode = 'r', perm = 0666, **opts, &block)
  end

  def eql?(other)
  end

  def instance_variable_names
  end

  def Integer(object, base = 0, exception: true)
  end

  def Float(arg, exception: true)
  end

  sig { returns(String) }
  def String(object)
  end

  def Array(object)
  end

  sig { returns(Hash) }
  def Hash(object)
  end

  def Rational(x, y = nil, exception: true)
  end

  def Complex(real, imag = 0, exception: true)
  end
end

class Object < BasicObject
  include Kernel

  def class
  end

  def <=>(other)
  end

  def ===(other)
  end

  def =~(other)
  end

  def !~(other)
  end
end

class Module < Object
  def initialize
  end

  def include(*modules)
  end

  def prepend(*modules)
  end

  def extend_object(obj)
  end

  def included(othermod)
  end

  def extended(othermod)
  end

  def prepended(othermod)
  end

  sig { returns(Array) }
  def attr_reader(*names)
  end

  sig { returns(Array) }
  def attr_writer(*names)
  end

  sig { returns(Array) }
  def attr_accessor(*names)
  end

  sig { returns(Array) }
  def attr(*names)
  end

  sig { returns(Symbol) }
  def define_method(symbol, method = nil, &block)
  end

  sig { returns(Symbol) }
  def alias_method(new_name, old_name)
  end

  def remove_method(symbol = nil, *names)
  end

  def undef_method(symbol = nil, *names)
  end

  def private(method_name = nil, arg2 = nil, *names)
  end

  def public(method_name = nil, arg2 = nil, *names)
  end

  def protected(method_name = nil, arg2 = nil, *names)
  end

  def module_function(method_name = nil, arg2 = nil, *names)
  end

  def private_constant(*names)
  end

  def public_constant(*names)
  end

  def private_class_method(array = nil, *names)
  end

  def public_class_method(array = nil, *names)
  end

  def const_get(sym, inherit = true)
  end

  def const_set(sym, obj)
  end

  def const_defined?(sym, inherit = true)
  end

  def const_missing(sym)
  end

  sig { returns(Array) }
  def constants(inherit = true)
  end

  def name
  end

  sig { returns(Array) }
  def ancestors
  end

  sig { returns(Array) }
  def included_modules
  end

  def include?(mod)
  end

  sig { returns(Array) }
  def instance_methods(include_super = true)
  end

  sig { returns(UnboundMethod) }
  def instance_method(symbol)
  end

  sig { returns(Array) }
  def public_instance_methods(include_super = true)
  end

  sig { returns(Array) }
  def private_instance_methods(include_super = true)
  end

  def method_defined?(symbol, inherit = true)
  end

  def private_method_defined?(symbol, inherit = true)
  end

  def instance_variable_get(symbol)
  end

  def module_eval(arg0 = nil, filename = nil, lineno = nil, &block)
  end

  def class_eval(*args, &block)
  end

  def module_exec(*args, **options, &block)
  end

  def class_exec(*args, **options, &block)
  end

  sig { returns(Symbol) }
  def define_singleton_method(symbol, method = nil, &block)
  end

  def <(other)
  end

  def <=(other)
  end

  def >(other)
  end

  def >=(other)
  end
end

class Class < Module
  def initialize(*args)
  end

  def inherited(subclass)
  end

  def new(*args, &block)
  end

  def allocate
  end

  def superclass
  end
end

module Comparable
  def <(other)
  end

  def <=(other)
  end

  def >(other)
  end

  def >=(other)
  end

  def ==(other)
  end

  def between?(min, max)
  end

  def clamp(min, max = nil)
  end
end

module Enumerable
  # Abstract in core, but every includer defines it and navigation asks about
  # it constantly, so it is worth naming.
  def each(&block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def each_entry(*args, &block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  sig { params(block: T.proc.void).returns(Array) }
  def map(&block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  sig { params(block: T.proc.void).returns(Array) }
  def collect(&block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  sig { params(block: T.proc.void).returns(Array) }
  def flat_map(&block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  sig { params(block: T.proc.void).returns(Array) }
  def collect_concat(&block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  sig { params(block: T.proc.void).returns(Array) }
  def select(&block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  sig { params(block: T.proc.void).returns(Array) }
  def filter(&block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  sig { params(block: T.proc.void).returns(Array) }
  def filter_map(&block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  sig { params(block: T.proc.void).returns(Array) }
  def reject(&block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def find(if_none_proc = nil, &block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def detect(ifnone = nil, &block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  sig { params(block: T.proc.void).returns(Array) }
  def find_all(&block)
  end

  def find_index(object = nil, &block)
  end

  def reduce(init = nil, method = nil, &block)
  end

  def inject(initial_value = nil, symbol = nil, &block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def each_with_index(*args, &block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def each_with_object(object, &block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def each_slice(n, &block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def each_cons(n, &block)
  end

  sig { returns(Array) }
  def sort(&block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  sig { params(block: T.proc.void).returns(Array) }
  def sort_by(&block)
  end

  sig { params(n: T.untyped).returns(Array) }
  def min(n = nil, &block)
  end

  sig { params(n: T.untyped).returns(Array) }
  def max(n = nil, &block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  sig { params(n: T.untyped, block: T.proc.void).returns(Array) }
  def min_by(n = nil, &block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  sig { params(n: T.untyped, block: T.proc.void).returns(Array) }
  def max_by(n = nil, &block)
  end

  def minmax(&block)
  end

  def sum(initial_value = 0, &block)
  end

  sig { returns(Integer) }
  def count(object = nil, &block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  sig { params(block: T.proc.void).returns(Hash) }
  def group_by(&block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def partition(&block)
  end

  sig { params(block: T.proc.void).returns(Enumerator) }
  def chunk_while(&block)
  end

  sig { params(block: T.proc.void).returns(Enumerator) }
  def slice_when(&block)
  end

  sig { returns(Hash) }
  def tally(hash = {})
  end

  sig { returns(Array) }
  def uniq(&block)
  end

  sig { params(block: NilClass).returns(Array) }
  def zip(*other_enums, &block)
  end

  sig { returns(Array) }
  def take(n)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  sig { params(block: T.proc.void).returns(Array) }
  def take_while(&block)
  end

  sig { returns(Array) }
  def drop(n)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  sig { params(block: T.proc.void).returns(Array) }
  def drop_while(&block)
  end

  sig { params(n: T.untyped).returns(Array) }
  def first(n = nil)
  end

  def include?(value)
  end

  def member?(value)
  end

  sig { returns(Array) }
  def to_a(*args)
  end

  sig { returns(Array) }
  def entries
  end

  sig { returns(Hash) }
  def to_h(*args, &block)
  end

  sig { returns(Set) }
  def to_set(klass = Set, *args, &block)
  end

  def lazy
  end

  def any?(pattern = nil, &block)
  end

  def all?(pattern = nil, &block)
  end

  def none?(pattern = nil, &block)
  end

  def one?(pattern = nil, &block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def each_entry(*args, &block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def reverse_each(*args, &block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  sig { params(block: T.proc.void).returns(NilClass) }
  def cycle(n = nil, &block)
  end

  def with_index(offset = 0, &block)
  end
end

class NilClass < Object
  def to_a
  end

  def to_s
  end

  def to_h
  end

  def nil?
  end

  def &(other)
  end

  def |(other)
  end
end

class TrueClass < Object; end
class FalseClass < Object; end

class Symbol < Object
  include Comparable

  sig { returns(Proc) }
  def to_proc
  end

  def to_sym
  end

  sig { returns(String) }
  def to_s
  end

  sig { returns(String) }
  def name
  end

  sig { returns(Integer) }
  def length
  end

  sig { returns(Symbol) }
  def upcase(*options)
  end

  sig { returns(Symbol) }
  def downcase(*options)
  end

  def start_with?(*string_or_regexp)
  end

  def end_with?(*strings)
  end

  def [](*args)
  end
end

class Numeric < Object
  include Comparable

  def +(other)
  end

  def -(other)
  end

  def *(other)
  end

  def /(other)
  end

  def %(other)
  end

  def **(other)
  end

  def abs
  end

  def round(digits = 0)
  end

  def floor(ndigits = 0)
  end

  def ceil(ndigits = 0)
  end

  def to_i
  end

  sig { returns(Integer) }
  def to_int
  end

  def to_f
  end

  def to_r
  end

  def zero?
  end

  def positive?
  end

  def negative?
  end

  def nonzero?
  end

  def coerce(other)
  end

  def divmod(other)
  end

  def clamp(min, max = nil)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def step(to = nil, by = 1, &block)
  end
end

class Integer < Numeric
  sig { params(block: NilClass).returns(Enumerator) }
  def times(&block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  sig { params(block: T.proc.void).returns(Integer) }
  def upto(limit, &block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  sig { params(block: T.proc.void).returns(Integer) }
  def downto(limit, &block)
  end

  sig { returns(Integer) }
  def succ
  end

  sig { returns(Integer) }
  def next
  end

  sig { returns(Integer) }
  def pred
  end

  def even?
  end

  def odd?
  end

  sig { returns(Integer) }
  def gcd(other_int)
  end

  sig { returns(Integer) }
  def lcm(other_int)
  end

  sig { returns(Array) }
  def digits(base = 10)
  end

  sig { returns(String) }
  def chr(encoding = nil)
  end

  sig { returns(Integer) }
  def ord
  end

  sig { returns(String) }
  def to_s(base = 10)
  end

  sig { returns(Float) }
  def fdiv(numeric)
  end

  sig { params(integer: T.untyped, integer2: T.untyped).returns(Integer) }
  def pow(integer, integer2 = nil)
  end

  sig { returns(Integer) }
  def bit_length
  end
end

class Float < Numeric
  def nan?
  end

  def infinite?
  end

  def finite?
  end

  def truncate(ndigits = 0)
  end
end

class Rational < Numeric; end
class Complex < Numeric; end

class String < Object
  def initialize(*args)
  end
  include Comparable

  sig { returns(String) }
  def +(other)
  end

  sig { returns(String) }
  def *(count)
  end

  sig { returns(String) }
  def %(args)
  end

  def <<(other)
  end

  def =~(other)
  end

  def [](*args)
  end

  def []=(*args)
  end

  sig { returns(Integer) }
  def length
  end

  sig { returns(Integer) }
  def size
  end

  sig { returns(Integer) }
  def bytesize
  end

  def empty?
  end

  sig { returns(String) }
  def to_s
  end

  sig { returns(String) }
  def to_str
  end

  sig { returns(Symbol) }
  def to_sym
  end

  sig { returns(Integer) }
  def to_i(base = 10)
  end

  sig { returns(Float) }
  def to_f
  end

  sig { returns(Rational) }
  def to_r
  end

  sig { returns(Complex) }
  def to_c
  end

  sig { returns(String) }
  def upcase(*options)
  end

  sig { returns(String) }
  def downcase(*options)
  end

  sig { returns(String) }
  def capitalize(*options)
  end

  sig { returns(String) }
  def swapcase(*options)
  end

  sig { returns(String) }
  def strip
  end

  sig { returns(String) }
  def lstrip
  end

  sig { returns(String) }
  def rstrip
  end

  sig { returns(String) }
  def chomp(line_sep = $/)
  end

  sig { returns(String) }
  def chop
  end

  sig { params(block: NilClass).returns(Array) }
  def chars(&block)
  end

  sig { params(block: NilClass).returns(Array) }
  def bytes(&block)
  end

  sig { params(block: NilClass).returns(Array) }
  def lines(separator = nil, chomp: nil, &block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def each_char(&block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def each_line(line_sep = $/, chomp: false, &block)
  end

  sig { params(block: NilClass).returns(Array) }
  def split(field_sep = $;, limit = 0, &block)
  end

  def join(*args)
  end

  sig { returns(String) }
  def sub(pattern, replacement = nil, &block)
  end

  sig { params(pattern: T.untyped, replacement: T.untyped, block: NilClass).returns(String) }
  sig { params(pattern: T.untyped, block: NilClass).returns(Enumerator) }
  sig { params(block: T.proc.void).returns(String) }
  def gsub(pattern, replacement = nil, &block)
  end

  def sub!(pattern, replacement = nil, &block)
  end

  sig { params(pattern: T.untyped, block: NilClass).returns(Enumerator) }
  def gsub!(pattern, replacement = nil, &block)
  end

  sig { returns(String) }
  def tr(selector, replacements)
  end

  sig { returns(String) }
  def delete(*selectors)
  end

  sig { returns(String) }
  def squeeze(*selectors)
  end

  def replace(other_string)
  end

  def insert(index, other_string)
  end

  def concat(*objects)
  end

  def prepend(*other_strings)
  end

  def start_with?(*string_or_regexp)
  end

  def end_with?(*strings)
  end

  def include?(other_string)
  end

  def index(substring, offset = 0)
  end

  def rindex(substring, offset = self.length)
  end

  def match(pattern, offset = 0, &block)
  end

  def match?(pattern, offset = 0)
  end

  sig { params(block: NilClass).returns(Array) }
  def scan(string_or_regexp, &block)
  end

  def slice(start, length = nil)
  end

  def slice!(start, length = nil)
  end

  sig { returns(String) }
  def center(size, pad_string = ' ')
  end

  sig { returns(String) }
  def ljust(size, pad_string = ' ')
  end

  sig { returns(String) }
  def rjust(size, pad_string = ' ')
  end

  sig { returns(String) }
  def reverse
  end

  def freeze
  end

  def frozen?
  end

  def dup
  end

  sig { returns(Integer) }
  def hash
  end

  sig { returns(String) }
  def inspect
  end

  sig { params(block: NilClass).returns(Array) }
  def unpack(template, offset: 0, &block)
  end

  def unpack1(template, offset: 0)
  end

  def encode(dst_encoding = nil, src_encoding = nil, **enc_opts)
  end

  def force_encoding(encoding)
  end

  sig { returns(Encoding) }
  def encoding
  end

  def valid_encoding?
  end

  def unicode_normalize(form = :nfc)
  end

  sig { returns(String) }
  def succ
  end

  sig { returns(String) }
  def next
  end

  sig { returns(Integer) }
  def ord
  end

  sig { returns(Integer) }
  def count(*selectors)
  end

  sig { returns(String) }
  def format(format, *args)
  end
end

class Array < Object
  def initialize(*args)
  end
  include Enumerable

  def [](*args)
  end

  def []=(*args)
  end

  def <<(value)
  end

  sig { returns(Array) }
  def +(other)
  end

  sig { returns(Array) }
  def -(other)
  end

  def *(other)
  end

  sig { returns(Array) }
  def &(other)
  end

  sig { returns(Array) }
  def |(other)
  end

  sig { returns(Integer) }
  def length
  end

  sig { returns(Integer) }
  def size
  end

  def empty?
  end

  def push(*objects)
  end

  def append(*objects)
  end

  sig { params(count: T.untyped).returns(Array) }
  def pop(count = nil)
  end

  sig { params(count: T.untyped).returns(Array) }
  def shift(count = nil)
  end

  def unshift(*objects)
  end

  def prepend(*objects)
  end

  def insert(index, *objects)
  end

  def delete(object, &block)
  end

  def delete_at(index)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def delete_if(&block)
  end

  def clear
  end

  def concat(*other_arrays)
  end

  sig { returns(Array) }
  def compact
  end

  def compact!
  end

  sig { returns(Array) }
  def flatten(depth = nil)
  end

  def flatten!(depth = nil)
  end

  sig { returns(Array) }
  def uniq(&block)
  end

  def uniq!(&block)
  end

  sig { returns(Array) }
  def reverse
  end

  sig { returns(Array) }
  def reverse!
  end

  sig { returns(Array) }
  def rotate(count = 1)
  end

  def sort!(&block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  sig { params(block: T.proc.void).returns(Array) }
  def sort_by!(&block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def select!(&block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def reject!(&block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def map!(&block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def collect!(&block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def each(&block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def each_index(&block)
  end

  sig { params(count: T.untyped).returns(Array) }
  def first(count = nil)
  end

  sig { params(count: T.untyped).returns(Array) }
  def last(count = nil)
  end

  sig { params(count: T.untyped).returns(Array) }
  def sample(count = nil, random: Random)
  end

  sig { returns(Array) }
  def shuffle(random: Random)
  end

  def slice(start, length = nil)
  end

  def slice!(start, length = nil)
  end

  def fill(object = nil, start = nil, count = nil, &block)
  end

  def dig(index, *identifiers)
  end

  sig { returns(Array) }
  def values_at(*specifiers)
  end

  def assoc(object)
  end

  def rassoc(value)
  end

  def index(object = nil, &block)
  end

  def rindex(object = nil, &block)
  end

  sig { returns(String) }
  def join(separator = nil)
  end

  sig { returns(String) }
  def pack(template, buffer: nil)
  end

  sig { returns(Array) }
  def product(*other_arrays, &block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def combination(count, &block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  sig { params(block: T.proc.void).returns(Array) }
  def permutation(count = self.size, &block)
  end

  sig { returns(Array) }
  def transpose
  end

  sig { returns(Array) }
  def to_a
  end

  def to_ary
  end

  sig { returns(Hash) }
  def to_h(&block)
  end

  def freeze
  end

  def frozen?
  end

  sig { returns(Integer) }
  def hash
  end

  def replace(other)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def bsearch(&block)
  end
end

class Hash < Object
  def initialize(ifnone = nil, capacity: 0, &block)
  end
  include Enumerable

  def [](key)
  end

  def []=(key, value)
  end

  def fetch(key, default_value = nil, &block)
  end

  def store(key, value)
  end

  def dig(key, *identifiers)
  end

  def delete(key, &block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def delete_if(&block)
  end

  sig { returns(Array) }
  def keys
  end

  sig { returns(Array) }
  def values
  end

  sig { returns(Array) }
  def values_at(*keys)
  end

  sig { returns(Array) }
  def fetch_values(*keys, &block)
  end

  def key?(key)
  end

  def has_key?(key)
  end

  def include?(key)
  end

  def member?(key)
  end

  def value?(value)
  end

  def has_value?(value)
  end

  def key(value)
  end

  sig { returns(Integer) }
  def length
  end

  sig { returns(Integer) }
  def size
  end

  def empty?
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def each(&block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def each_pair(&block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  sig { params(block: T.proc.void).returns(Hash) }
  def each_key(&block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def each_value(&block)
  end

  sig { returns(Hash) }
  def merge(*other_hashes, &block)
  end

  def merge!(*others, &block)
  end

  def update(*others, &block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  sig { params(block: T.proc.void).returns(Hash) }
  def transform_keys(hash2 = nil, &block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  sig { params(block: T.proc.void).returns(Hash) }
  def transform_values(&block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def transform_keys!(hash2 = nil, &block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def transform_values!(&block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def select!(&block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def reject!(&block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def keep_if(&block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  sig { params(block: T.proc.void).returns(Array) }
  def filter_map(&block)
  end

  sig { returns(Hash) }
  def slice(*keys)
  end

  sig { returns(Hash) }
  def except(*keys)
  end

  sig { returns(Hash) }
  def compact
  end

  def compact!
  end

  sig { returns(Hash) }
  def invert
  end

  sig { returns(Hash) }
  def to_h(&block)
  end

  sig { returns(Array) }
  def to_a
  end

  def default(key = nil)
  end

  def default=(value)
  end

  def default_proc
  end

  def clear
  end

  def freeze
  end

  def frozen?
  end

  def replace(other)
  end

  def any?(object = nil, &block)
  end

  def sum(initial_value = 0, &block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  sig { params(block: T.proc.void).returns(Hash) }
  def group_by(&block)
  end
end

class Range < Object
  def initialize(*args)
  end
  include Enumerable

  def begin
  end

  def end
  end

  sig { params(n: T.untyped).returns(Array) }
  def first(n = nil)
  end

  sig { params(n: T.untyped).returns(Array) }
  def last(n = nil)
  end

  sig { params(n: T.untyped).returns(Array) }
  def min(n = nil, &block)
  end

  sig { params(n: T.untyped).returns(Array) }
  def max(n = nil, &block)
  end

  def size
  end

  sig { returns(Integer) }
  def count(object = nil, &block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def step(s = 1, &block)
  end

  def cover?(object)
  end

  def include?(obj)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def each(&block)
  end

  sig { returns(Array) }
  def to_a(*args)
  end

  def exclude_end?
  end
end

class Struct < Object
  def initialize(*args)
  end
  include Enumerable

  def self.new(*args, &block)
  end

  sig { returns(Array) }
  def members
  end

  sig { returns(Array) }
  def to_a
  end

  sig { returns(Hash) }
  def to_h(&block)
  end

  def [](key)
  end

  def []=(key, value)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def each(&block)
  end

  def dig(name, *identifiers)
  end

  sig { returns(Array) }
  def deconstruct
  end

  sig { returns(Hash) }
  def deconstruct_keys(array_of_names)
  end
end

class Data < Object
  def initialize(*args)
  end

  def self.define(*symbols, &block)
  end

  def with(**kwargs)
  end

  sig { returns(Hash) }
  def to_h(&block)
  end

  sig { returns(Array) }
  def members
  end

  sig { returns(Array) }
  def deconstruct
  end

  sig { returns(Hash) }
  def deconstruct_keys(array_of_names_or_nil = nil)
  end
end

class Set < Object
  def initialize(enum = nil, &block)
  end
  include Enumerable

  def add(o)
  end

  def <<(value)
  end

  def add?(o)
  end

  def delete(o)
  end

  def include?(o)
  end

  def member?(value)
  end

  sig { returns(Integer) }
  def size
  end

  sig { returns(Integer) }
  def length
  end

  def empty?
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def each(&block)
  end

  sig { returns(Array) }
  def to_a
  end

  def merge(*others)
  end

  def subset?(set)
  end

  def superset?(set)
  end

  def |(other)
  end

  def &(other)
  end

  def -(other)
  end

  def freeze
  end
end

class Enumerator < Object
  def initialize(*args)
  end
  include Enumerable

  def next
  end

  def peek
  end

  def rewind
  end

  def size
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def with_index(offset = 0, &block)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def with_object(obj, &block)
  end

  def each(*appending_args, &block)
  end

  class Lazy < Enumerator; end
  class Yielder < Object
    def <<(value)
    end

    def yield(*args)
    end
  end
end

class Proc < Object
  def call(*args, &block)
  end

  def ===(*args)
  end

  def [](*args)
  end

  def yield(*args)
  end

  sig { returns(Integer) }
  def arity
  end

  def lambda?
  end

  sig { returns(Proc) }
  def curry(arity = nil)
  end

  def to_proc
  end

  def parameters(lambda: nil)
  end
end

class Method < Object
  def call(*args, &block)
  end

  sig { returns(Proc) }
  def to_proc
  end

  sig { returns(Integer) }
  def arity
  end

  sig { returns(Symbol) }
  def name
  end

  def owner
  end

  def receiver
  end

  def parameters
  end

  def source_location
  end

  sig { returns(UnboundMethod) }
  def unbind
  end
end

class UnboundMethod < Object
  sig { returns(Method) }
  def bind(obj)
  end

  sig { returns(Symbol) }
  def name
  end

  def owner
  end

  sig { returns(Integer) }
  def arity
  end

  def source_location
  end
end

class Binding < Object
  def local_variable_get(symbol)
  end

  def local_variable_set(symbol, obj)
  end

  sig { returns(Array) }
  def local_variables
  end

  def receiver
  end

  def eval(src, filename = nil, lineno = nil)
  end
end

class Regexp < Object
  def initialize(*args)
  end

  def match(string, offset = 0, &block)
  end

  def match?(string, offset = 0)
  end

  def =~(other)
  end

  def ===(other)
  end

  sig { returns(String) }
  def source
  end

  sig { returns(Integer) }
  def options
  end

  sig { returns(Array) }
  def names
  end

  sig { returns(String) }
  def self.escape(string)
  end

  sig { returns(Regexp) }
  def self.union(array_of_patterns = nil, *patterns)
  end

  def self.last_match(n = nil)
  end
end

class MatchData < Object
  def [](*args)
  end

  sig { returns(Array) }
  def captures
  end

  sig { returns(Hash) }
  def named_captures(symbolize_names: false)
  end

  sig { returns(Array) }
  def names
  end

  sig { returns(String) }
  def pre_match
  end

  sig { returns(String) }
  def post_match
  end

  sig { returns(Array) }
  def to_a
  end

  def begin(n)
  end

  def end(n)
  end
end

class Exception < Object
  def initialize(*args)
  end

  sig { returns(String) }
  def message
  end

  sig { returns(String) }
  def to_s
  end

  sig { returns(String) }
  def full_message(highlight: true, order: :top)
  end

  def backtrace
  end

  def backtrace_locations
  end

  def cause
  end

  def exception(message = nil)
  end

  def self.exception(message = nil)
  end
end

class ScriptError < Exception; end
class LoadError < ScriptError; end
class NotImplementedError < ScriptError; end
class SyntaxError < ScriptError
  def initialize(*args)
  end
end
class NoMemoryError < Exception; end
class SecurityError < Exception; end
class SystemExit < Exception
  def initialize(*args)
  end
end
class SignalException < Exception
  def initialize(*args)
  end
end
class Interrupt < SignalException
  def initialize(*args)
  end
end
class SystemStackError < Exception; end

class StandardError < Exception; end
class RuntimeError < StandardError; end
class FrozenError < RuntimeError
  def initialize(*args)
  end
end
class ArgumentError < StandardError; end
class TypeError < StandardError; end
class NameError < StandardError
  def initialize(*args)
  end

  def name
  end

  def receiver
  end
end
class NoMethodError < NameError
  def initialize(*args)
  end

  sig { returns(Array) }
  def args
  end
end
class IndexError < StandardError; end
class KeyError < IndexError
  def initialize(*args)
  end

  def key
  end

  def receiver
  end
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
class NoMatchingPatternKeyError < NoMatchingPatternError
  def initialize(*args)
  end
end
class UncaughtThrowError < ArgumentError
  def initialize(*args)
  end
end
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
  def initialize(*args)
  end
  include Enumerable

  def read(maxlen = nil, out_string = nil)
  end

  sig { returns(Integer) }
  def write(*objects)
  end

  def puts(*objects)
  end

  def print(*objects)
  end

  def printf(format_string, *objects)
  end

  def gets(sep = nil, limit = nil, chomp: false)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def each_line(sep = nil, limit = nil, chomp: nil, &block)
  end

  sig { returns(Array) }
  def readlines(sep = nil, limit = nil, chomp: false)
  end

  sig { returns(String) }
  def readline(sep = nil, limit = nil, chomp: false)
  end

  def close
  end

  def closed?
  end

  def flush
  end

  def sync
  end

  def sync=(boolean)
  end

  sig { returns(Integer) }
  def fileno
  end

  def eof?
  end

  sig { returns(Integer) }
  def rewind
  end

  sig { returns(Integer) }
  def seek(offset, whence = IO::SEEK_SET)
  end

  sig { returns(Integer) }
  def pos
  end
end

class File < IO
  def initialize(*args)
  end

  sig { returns(String) }
  def self.read(path, length = nil, offset = 0, **opts)
  end

  sig { returns(Integer) }
  def self.write(path, data, offset = 0, **opts)
  end

  def self.open(path, mode = 'r', perm = 0666, **opts, &block)
  end

  def self.exist?(file_name)
  end

  def self.exists?(path)
  end

  def self.file?(file)
  end

  def self.directory?(path)
  end

  def self.readable?(file_name)
  end

  def self.writable?(file_name)
  end

  def self.executable?(file_name)
  end

  sig { returns(Integer) }
  def self.size(file_name)
  end

  def self.size?(file_name)
  end

  def self.zero?(file_name)
  end

  sig { returns(Integer) }
  def self.delete(*paths)
  end

  sig { returns(Integer) }
  def self.unlink(*paths)
  end

  def self.rename(old_name, new_name)
  end

  sig { returns(String) }
  def self.join(*parts)
  end

  sig { returns(String) }
  def self.expand_path(file_name, dir_string = nil)
  end

  sig { returns(String) }
  def self.absolute_path(file_name, dir_string = nil)
  end

  sig { returns(String) }
  def self.basename(file_name, suffix = nil)
  end

  sig { returns(String) }
  def self.dirname(file_name, level = 1)
  end

  sig { returns(String) }
  def self.extname(path)
  end

  def self.split(file_name)
  end

  sig { returns(Array) }
  def self.readlines(path, sep = nil, limit = nil, **opts)
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def self.foreach(path, sep = nil, limit = nil, **opts, &block)
  end

  sig { returns(String) }
  def self.binread(path, length = nil, offset = 0)
  end

  sig { returns(Integer) }
  def self.binwrite(path, string, offset = 0)
  end

  sig { returns(Time) }
  def self.mtime(file_name)
  end

  sig { returns(Time) }
  def self.ctime(file_name)
  end

  sig { returns(Time) }
  def self.atime(file_name)
  end

  def self.stat(filepath)
  end

  def self.symlink?(filepath)
  end

  sig { returns(String) }
  def self.realpath(pathname, dir_string = nil)
  end

  sig { returns(String) }
  def path
  end
end

class Dir < Object
  def initialize(name, encoding: nil)
  end
  include Enumerable

  sig { params(block: NilClass).returns(Array) }
  def self.glob(*patterns, flags: 0, base: nil, sort: true, &block)
  end

  sig { returns(Array) }
  def self.[](*args)
  end

  sig { returns(Array) }
  def self.entries(dirname, encoding: 'UTF-8')
  end

  sig { returns(Array) }
  def self.children(dirpath, encoding: 'UTF-8')
  end

  sig { params(block: NilClass).returns(Enumerator) }
  def self.each_child(dirpath, encoding: 'UTF-8', &block)
  end

  def self.mkdir(dirpath, permissions = 0775)
  end

  def self.rmdir(dirpath)
  end

  def self.exist?(dirpath)
  end

  sig { returns(String) }
  def self.pwd
  end

  def self.chdir(new_dirpath = nil, &block)
  end

  sig { returns(String) }
  def self.home(user_name = nil)
  end

  def self.tmpdir
  end
end

class Time < Object
  def initialize(*args)
  end
  include Comparable

  sig { returns(Time) }
  def self.now(in: nil)
  end

  sig { returns(Time) }
  def self.at(*args)
  end

  def self.parse(*args)
  end

  sig { returns(Time) }
  def self.new(*args)
  end

  sig { returns(Integer) }
  def year
  end

  sig { returns(Integer) }
  def month
  end

  sig { returns(Integer) }
  def day
  end

  sig { returns(Integer) }
  def hour
  end

  sig { returns(Integer) }
  def min
  end

  sig { returns(Integer) }
  def sec
  end

  sig { returns(Integer) }
  def usec
  end

  sig { returns(Integer) }
  def nsec
  end

  sig { returns(Integer) }
  def wday
  end

  sig { returns(Integer) }
  def yday
  end

  def zone
  end

  sig { returns(Integer) }
  def to_i
  end

  sig { returns(Float) }
  def to_f
  end

  sig { returns(Rational) }
  def to_r
  end

  sig { returns(String) }
  def to_s
  end

  sig { returns(Time) }
  def utc
  end

  def utc?
  end

  sig { returns(Time) }
  def localtime(zone = nil)
  end

  sig { returns(Time) }
  def getlocal(zone = nil)
  end

  sig { returns(String) }
  def strftime(format_string)
  end

  sig { returns(Time) }
  def +(other)
  end

  def -(other)
  end
end

class Random < Object
  def self.rand(max = nil)
  end

  sig { returns(Integer) }
  def self.new_seed
  end

  sig { returns(Integer) }
  def self.srand(number = nil)
  end

  def rand(max = nil)
  end

  sig { returns(Integer) }
  def seed
  end

  sig { returns(String) }
  def bytes(size)
  end
end

class Thread < Object
  def initialize(*args)
  end

  sig { params(block: T.proc.void).returns(Thread) }
  def self.new(*args, &block)
  end

  sig { returns(Thread) }
  def self.current
  end

  sig { returns(Thread) }
  def self.main
  end

  def self.list
  end

  sig { returns(Thread) }
  def join(limit = nil, *args)
  end

  def value
  end

  def alive?
  end

  def kill
  end

  def [](key)
  end

  def []=(key, value)
  end

  sig { returns(String) }
  def name
  end

  def name=(name)
  end
end

class Mutex < Object
  def initialize
  end

  def lock
  end

  def unlock
  end

  def locked?
  end

  def synchronize(&block)
  end

  def try_lock
  end
end

class Queue < Object
  def initialize(*args)
  end

  def push(value)
  end

  def <<(value)
  end

  def pop(non_block = false)
  end

  def size
  end

  def length
  end

  def empty?
  end

  def close
  end

  def closed?
  end
end

class SizedQueue < Queue
  def initialize(max)
  end
end
class ConditionVariable < Object
  def initialize
  end

  def wait(mutex, timeout = nil)
  end

  def signal
  end

  def broadcast
  end
end

class Fiber < Object
  def initialize(*args)
  end

  def self.yield(*args)
  end

  sig { params(block: T.proc.void).returns(Fiber) }
  def self.new(&block)
  end

  def resume(*args)
  end

  def alive?
  end
end

class Ractor < Object; end

module Math
  sig { returns(Float) }
  def self.sqrt(x)
  end

  sig { returns(Float) }
  def self.cbrt(x)
  end

  sig { returns(Float) }
  def self.log(x, base = Math::E)
  end

  sig { returns(Float) }
  def self.log2(x)
  end

  sig { returns(Float) }
  def self.log10(x)
  end

  sig { returns(Float) }
  def self.exp(x)
  end

  sig { returns(Float) }
  def self.sin(x)
  end

  sig { returns(Float) }
  def self.cos(x)
  end

  sig { returns(Float) }
  def self.tan(x)
  end

  sig { returns(Float) }
  def self.atan(x)
  end

  sig { returns(Float) }
  def self.atan2(y, x)
  end

  sig { returns(Float) }
  def self.hypot(a, b)
  end

  def self.pow(x, y)
  end
end

module ObjectSpace
  sig { params(block: NilClass).returns(Enumerator) }
  sig { params(block: T.proc.void).returns(Integer) }
  def self.each_object(mod = nil, &block)
  end

  def self.garbage_collect(full_mark: true, immediate_mark: true, immediate_sweep: true)
  end

  def self.define_finalizer(obj, aProc = nil, &block)
  end

  sig { returns(Hash) }
  def self.count_objects(result_hash = nil)
  end
end

module GC
  def self.start(full_mark: true, immediate_mark: true, immediate_sweep: true)
  end

  def self.stat(hash = nil)
  end

  def self.disable
  end

  def self.enable
  end

  def self.compact
  end
end

module Marshal
  sig { params(obj: T.untyped).returns(String) }
  def self.dump(obj, port = nil, limit = nil)
  end

  def self.load(source, proc = nil, freeze: false)
  end
end

module Process
  sig { returns(Integer) }
  def self.pid
  end

  sig { returns(Integer) }
  def self.ppid
  end

  def self.exit(status = true)
  end

  def self.exit!(status = false)
  end

  sig { params(block: T.proc.void).returns(Integer) }
  def self.fork(&block)
  end

  sig { returns(Integer) }
  def self.wait(pid = -1, flags = 0)
  end

  sig { returns(Integer) }
  def self.spawn(env, command = nil, *args, unsetenv_others: nil, pgroup: nil, umask: nil, in: nil, out: nil, err: nil, close_others: nil, chdir: nil)
  end

  sig { returns(Integer) }
  def self.kill(signal, *ids)
  end

  sig { params(clock_id: T.untyped).returns(Float) }
  def self.clock_gettime(clock_id, unit = :float_second)
  end
end

module Signal
  def self.trap(signal, command = nil, &block)
  end

  sig { returns(Hash) }
  def self.list
  end
end

module Warning
  def self.warn(msg, category: nil)
  end
end

class Encoding < Object
  sig { returns(Encoding) }
  def self.default_external
  end

  def self.default_internal
  end

  sig { returns(String) }
  def name
  end
end

module FileUtils
  def self.mkdir_p(*args)
  end

  def self.rm_rf(*args)
  end

  def self.rm_f(*args)
  end

  def self.cp(*args)
  end

  def self.cp_r(*args)
  end

  def self.mv(*args)
  end

  def self.touch(*args)
  end

  def self.ln_s(*args)
  end
end

ENV = nil
ARGV = nil
RUBY_VERSION = nil
RUBY_PLATFORM = nil
RUBY_ENGINE = nil
