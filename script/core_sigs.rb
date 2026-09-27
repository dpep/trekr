#!/usr/bin/env ruby
# frozen_string_literal: true

# Rewrite src/tree/core.rb's method stubs from Ruby's own signatures.
#
#   ruby script/core_sigs.rb            # rewrites src/tree/core.rb in place
#
# core.rb decides *which* methods exist; this fills in *how they look*:
#
# - parameter names from the method's rdoc call-seq (`downcase(*options)`),
#   falling back to RBS, then to what the stub already said;
# - a Sorbet `sig` wherever RBS gives a return type that is one class. A
#   return that depends on the call — `map` returns an Enumerator without a
#   block, `gsub(pattern)` does too — becomes one `sig` per call shape, which
#   the extractor reads as overloads (DEC-077). Unions, `bool`, optionals and
#   element types (`first` → `Elem`) get no `sig` at all.
#
# Idempotent: a stub it already wrote is read back and rewritten the same.
# Needs the rbs gem Ruby 3.4 bundles (3.8.x). A method RBS does not know keeps
# its hand-written parameters and gets no `sig`.

gem "rbs", "~> 3.8.0"
require "rbs"

CORE = File.expand_path("../src/tree/core.rb", __dir__)

ENV_RBS = RBS::Environment.from_loader(RBS::EnvironmentLoader.new).resolve_type_names
BUILDER = RBS::DefinitionBuilder.new(env: ENV_RBS)

IDENT = /\A[a-z_][a-zA-Z0-9_]*\z/

# The return classes worth writing down: declared in core.rb, since the tree
# could never resolve another, and with no subclass there, since `Numeric#+`
# "returns a Numeric" and an Integer then looks up `to_s` in the wrong class.
# Enumerator is the exception: only `lazy` makes its subclass.
def returnable(lines)
  declared = lines.filter_map { |line| line[/^\s*(?:class|module) ([A-Z]\w*)/, 1] }.to_set
  parents = lines.filter_map { |line| line[/^\s*class [A-Z]\w* < ([A-Z]\w*)/, 1] }.to_set
  # A class object answers with its own singleton methods, which an instance
  # of Class has none of: `record.class.find` is not `Class#find`.
  declared - (parents - ["Enumerator"]) - %w[Class Module]
end

def definition(owner, singleton)
  name = RBS::TypeName.parse("::#{owner}")
  return unless ENV_RBS.class_decls.key?(name)

  singleton ? BUILDER.build_singleton(name) : BUILDER.build_instance(name)
rescue StandardError
  nil
end

def rbs_method(owner, singleton, name)
  definition(owner, singleton)&.methods&.[](name.to_sym)
end

# The one class a return type denotes, or nil.
def class_of(type, known)
  # `self` is the receiver's own class, which a subclass makes something
  # other than the owner — the IDENTITY rule already carries the receiver.
  return unless type.is_a?(RBS::Types::ClassInstance)

  name = type.name.to_s.delete_prefix("::")
  name if known.include?(name)
end

# (block given?, positional count) → the return class every covering overload
# agrees on; :none where nothing covers the shape, nil where they disagree.
def return_table(method, known)
  overloads = method.method_types.select { |o| o.type.is_a?(RBS::Types::Function) }
  # Past the longest finite form, every count behaves like the last one.
  ceiling = overloads.map { |o| o.type.required_positionals.size + o.type.optional_positionals.size }.max.to_i + 1
  table = Hash.new { |h, k| h[k] = [] }
  overloads.each do |overload|
    fn = overload.type
    low = fn.required_positionals.size + fn.trailing_positionals.size
    high = fn.rest_positionals ? ceiling : low + fn.optional_positionals.size
    blocks =
      if overload.block.nil? then [false]
      elsif overload.block.required then [true]
      else [false, true]
      end
    returns = class_of(fn.return_type, known)
    blocks.product((low..high).to_a).each { |shape| table[shape] << returns }
    blocks.each { |block| table[[block, :rest]] << returns } if fn.rest_positionals
  end
  table.transform_values { |returns| returns.uniq.size == 1 ? returns.first : nil }
end

# One block state's sigs as `[count or nil, class]` pairs: nil when they
# cannot be said, :absent when no overload covers the state at all. A state
# RBS types only at some counts is a `Partial` of those counts.
Partial = Struct.new(:shapes)

def shape_sigs(table, block, positional_names)
  cells = table.select { |(given, _), _| given == block }
  return :absent if cells.empty?
  # A shape RBS cannot type leaves only the counts that can be named: a sig
  # naming none, or the rest, would read as covering it (`first` would be an
  # Array at every count). Whether even those can be said depends on the
  # other state, so the caller decides.
  if cells.values.any?(&:nil?)
    return Partial.new(
      cells.filter_map do |(_, argc), returns|
        [argc, returns] if returns && argc != :rest && argc.between?(1, positional_names.size)
      end
    )
  end

  values = cells.values.uniq
  return [[nil, values.first]] if values.size == 1
  # Per count, which needs the counts to be finite and nameable, and a zero
  # count to have nothing to say: a `sig` naming no positional parameter means
  # "any count" (DEC-077).
  return if cells.key?([block, :rest])
  return if cells[[block, 0]]

  cells.map do |(_, argc), returns|
    return if argc > positional_names.size

    [argc, returns]
  end
end

def sigs_for(method, known, params)
  table = return_table(method, known)
  return [] if table.empty?

  positional = params.select { |p| %i[req opt].include?(p[:kind]) }.map { |p| p[:name] }
  block_param = params.find { |p| p[:kind] == :block }&.fetch(:name)
  without = shape_sigs(table, false, positional)
  with = shape_sigs(table, true, positional)

  render = lambda do |(argc, returns), block|
    names = argc ? positional.first(argc).map { |n| "#{n}: T.untyped" } : []
    names << "#{block_param}: #{block ? 'T.proc.void' : 'NilClass'}" unless block.nil?
    names.empty? ? "sig { returns(#{returns}) }" : "sig { params(#{names.join(', ')}).returns(#{returns}) }"
  end
  # A block changes nothing — or the method takes none, and Ruby ignores it.
  if with == :absent || with == without
    return [] unless without.is_a?(Array)

    return without.map { |shape| render.(shape, nil) }
  end
  # Only `block: NilClass` confines a sig to its block state and count; a
  # lone `T.proc` one is Sorbet's ordinary sig and would cover every call.
  # So a partial state is said only beside a blockless sig that confines the
  # whole set to overloads (`max_by(n) { }` is an Array, `max_by { }` is not).
  return [] if without.nil?

  shapes = ->(state) { state.is_a?(Partial) ? state.shapes : state }
  if [without, with].any?(Partial) && !(block_param && shapes.(without).is_a?(Array) && shapes.(without).any?)
    return []
  end

  [[without, false], [with, true]].flat_map do |state, block|
    list = shapes.(state)
    list.is_a?(Array) ? list.map { |shape| render.(shape, block) } : []
  end
end

# --- parameters -------------------------------------------------------------

KEYWORDS = %w[true false nil self class module def end if unless do then in begin].freeze
RENAMED = { "class" => "klass", "module" => "mod" }.freeze
# A default worth copying from a call-seq; anything else becomes `nil`.
DEFAULT = %r{\A(?:nil|true|false|-?\d+(?:\.\d+)?|'[^']*'|"[^"]*"|:\w+|\$[/;,>]|\{\}|\[\]|[A-Z][\w:]*|self\.\w+)\z}

def parse_params(text)
  return [] if text.nil? || text.strip.empty?

  parts = []
  depth = 0
  current = +""
  text.gsub("$,", "\0").each_char do |c|
    depth += 1 if "([{".include?(c)
    depth -= 1 if ")]}".include?(c)
    if c == "," && depth.zero?
      parts << current.strip
      current = +""
    else
      current << c
    end
  end
  parts << current.strip unless current.strip.empty?
  parts.map do |part|
    part = part.gsub("\0", "$,")
    param =
      case part
      when /\A&(\w+)\z/ then { kind: :block, name: $1 }
      when /\A\*\*(\w+)\z/ then { kind: :keyrest, name: $1 }
      when /\A\*(\w+)\z/ then { kind: :rest, name: $1 }
      when /\A(\w+):\z/ then { kind: :keyreq, name: $1 }
      when /\A(\w+):\s*(.+)\z/ then { kind: :key, name: $1, default: $2 }
      when /\A(\w+)\s*=\s*(.+)\z/ then { kind: :opt, name: $1, default: $2 }
      when /\A(\w+)\z/ then { kind: :req, name: $1 }
      else return nil
      end
    return nil unless param[:name].match?(IDENT) && !KEYWORDS.include?(param[:name])

    param[:default] = nil unless param[:default].to_s.match?(DEFAULT)
    param
  end
end

def render_params(params)
  params.map do |p|
    case p[:kind]
    when :req then p[:name]
    when :opt then "#{p[:name]} = #{p[:default] || 'nil'}"
    when :rest then "*#{p[:name]}"
    when :key then "#{p[:name]}: #{p[:default] || 'nil'}"
    when :keyreq then "#{p[:name]}:"
    when :keyrest then "**#{p[:name]}"
    when :block then "&#{p[:name]}"
    end
  end.join(", ")
end

# The text inside the parentheses that open `text`, balanced.
def parenthesized(text)
  return unless text.start_with?("(")

  depth = 0
  text.each_char.with_index do |c, i|
    depth += 1 if c == "("
    depth -= 1 if c == ")"
    return text[1...i] if depth.zero?
  end
  nil
end

# Every form the rdoc call-seq lists for this method, as parameter lists.
def call_seq_forms(method, name)
  text = method.comments.map(&:string).join("\n")
  block = text[/<!--(.*?)-->/m, 1] or return []
  block.lines.filter_map do |line|
    form = line[/^\s*-\s*(.*?)\s*(?:->|$)/, 1] or next
    form = form.sub(/\A\w+(?:::\w+)*\./, "") # `File.join(...)`
    next unless form.start_with?(name)

    rest = form.delete_prefix(name)
    next if rest.match?(/\A[\w?!=]/) # a longer name that shares the prefix

    args = rest.start_with?("(") ? parenthesized(rest) : ""
    next if args.nil?

    params = parse_params(args) or next
    has_block = rest.delete_prefix("(#{args})").include?("{")
    params << { kind: :block, name: "block" } if has_block && params.none? { |p| p[:kind] == :block }
    params
  end
end

# The union of the forms: every slot any form names, required only where
# every form requires it.
def merge_forms(forms)
  positional = forms.map { |f| f.select { |p| %i[req opt].include?(p[:kind]) } }
  widest = positional.max_by(&:size) || []
  required = positional.map { |f| f.count { |p| p[:kind] == :req } }.min || 0
  merged = widest.each_with_index.map do |p, i|
    i < required ? { kind: :req, name: p[:name] } : { kind: :opt, name: p[:name], default: p[:default] }
  end
  flat = forms.flatten
  rest = flat.find { |p| p[:kind] == :rest }
  merged << rest if rest
  flat.select { |p| p[:kind] == :key }.uniq { |p| p[:name] }.each { |p| merged << p }
  keyrest = flat.find { |p| p[:kind] == :keyrest }
  merged << keyrest if keyrest
  block = flat.find { |p| p[:kind] == :block }
  merged << block if block
  merged
end

SYNTHETIC = /\Aarg\d*\z/

# Parameters from RBS alone, when the call-seq has none worth reading.
def rbs_params(method, existing)
  fns = functions(method)
  return existing if fns.empty?

  # The widest overload names its slots most consistently: `raise`'s
  # `(exception, message, backtrace)` rather than `(message, …)` from another.
  slots = fns.map { |f| f.required_positionals + f.optional_positionals }.sort_by { |slot| -slot.size }
  names = slots.first.each_index.map do |i|
    slots.filter_map { |slot| slot[i]&.name&.to_s }.find { |n| !n.match?(SYNTHETIC) }
  end
  # A name RBS does not give is better left as the stub wrote it than invented.
  return existing if names.any?(&:nil?) || names.uniq.size < names.size

  params = names.map { |name| { kind: :opt, name: } }
  fns.flat_map { |f| f.required_keywords.keys + f.optional_keywords.keys }.uniq.each do |key|
    required = fns.all? { |f| f.required_keywords.key?(key) }
    params << { kind: required ? :keyreq : :key, name: key.to_s }
  end
  keyrest = fns.map(&:rest_keywords).compact.first
  params << { kind: :keyrest, name: keyrest.name&.to_s || "options" } if keyrest
  params
end

def functions(method)
  method.method_types.map(&:type).grep(RBS::Types::Function)
end

# Make a parameter list agree with what RBS says Ruby accepts: required only
# as far as every overload requires, variadic when any overload is, a block
# when any takes one, and every name distinct.
def reconcile(params, method, existing)
  fns = functions(method)
  unless fns.empty?
    lead = fns.map { |f| f.required_positionals.size }.min
    params.select { |p| %i[req opt].include?(p[:kind]) }.each_with_index do |p, i|
      p[:kind] = i < lead ? :req : :opt
    end
    # A form the call-seq could not spell (`instance_eval(string [, filename])`)
    # still takes its arguments.
    widest = fns.map { |f| f.required_positionals + f.optional_positionals }.max_by(&:size)
    taken = params.count { |p| %i[req opt].include?(p[:kind]) }
    if taken < widest.size && params.none? { |p| p[:kind] == :rest }
      at = params.index { |p| !%i[req opt].include?(p[:kind]) } || params.size
      extra = widest.drop(taken).each_with_index.map do |slot, i|
        { kind: :opt, name: slot.name&.to_s || "arg#{taken + i + 1}" }
      end
      params.insert(at, *extra)
    end
    if fns.any?(&:rest_positionals) && params.none? { |p| p[:kind] == :rest }
      name = existing.find { |p| p[:kind] == :rest }&.fetch(:name)
      name ||= fns.filter_map { |f| f.rest_positionals&.name&.to_s }.find { |n| !n.match?(SYNTHETIC) } || "args"
      at = params.index { |p| %i[key keyreq keyrest block].include?(p[:kind]) } || params.size
      params.insert(at, { kind: :rest, name: })
    end
  end
  if method.method_types.any?(&:block) && params.none? { |p| p[:kind] == :block }
    params << { kind: :block, name: "block" }
  end
  positional = params.select { |p| %i[req opt rest].include?(p[:kind]) }.map { |p| p[:name] }
  params.reject! { |p| %i[key keyreq].include?(p[:kind]) && positional.include?(p[:name]) }
  # A keyword may name a keyword argument (`in:`), never a positional one.
  params.each do |p|
    next if %i[key keyreq].include?(p[:kind]) || !KEYWORDS.include?(p[:name])

    p[:name] = RENAMED.fetch(p[:name], "#{p[:name]}_")
  end
  names = Hash.new(0)
  params.each do |p|
    names[p[:name]] += 1
    next unless names[p[:name]] > 1

    p[:name] = p[:kind] == :rest ? "args" : "#{p[:name]}#{names[p[:name]]}"
  end
  params
end

# `initialize` and `new` are what every class overrides, so the core stub's
# variadic signature is the honest one.
VARIADIC = %w[initialize new].freeze

def params_for(method, name, existing)
  return existing if VARIADIC.include?(name)
  # Operators read better as the stub wrote them (`+(other)`) than as a
  # call-seq (`string + other_string`) can say.
  return existing unless name.match?(/\A[a-z_]\w*[?!=]?\z/i)

  forms = call_seq_forms(method, name)
  params = forms.empty? ? rbs_params(method, existing) : merge_forms(forms)
  reconcile(params.map(&:dup), method, existing)
end

# --- the rewrite --------------------------------------------------------------

DEF = /\A(?<indent>\s*)def (?<self>self\.)?(?<name>[^\s(;]+)(?:\((?<params>.*)\))?(?<oneline>; end)?\s*\z/

def rewrite(source)
  lines = source.lines(chomp: true)
  known = returnable(lines)
  out = []
  owners = []
  i = 0
  while i < lines.size
    line = lines[i]
    if line.match?(/\A\s*sig \{.*\}\s*\z/)
      i += 1
      next
    end
    if (m = line.match(/\A(\s*)(class|module) ([A-Z][\w:]*)/)) && !line.include?("; end")
      owners.push([m[1].size, [owners.last&.last, m[3]].compact.join("::")])
    elsif line.match?(/\A\s*end\s*\z/) && owners.last && owners.last.first == line[/\A\s*/].size
      owners.pop
    end
    m = line.match(DEF)
    unless m && owners.any?
      out << line
      i += 1
      next
    end
    # A multi-line stub's `end`.
    i += 1 unless m[:oneline]
    i += 1

    owner = owners.last.last
    singleton = !m[:self].nil?
    existing = parse_params(m[:params]) || []
    method = rbs_method(owner, singleton, m[:name])
    params = method ? params_for(method, m[:name], existing) : existing
    sigs = method ? sigs_for(method, known, params) : []

    out << "" unless out.last.to_s.strip.empty? || out.last.to_s.match?(/\A\s*(class|module) /) || out.last.to_s.strip.start_with?("#")
    indent = m[:indent]
    sigs.each { |sig| out << "#{indent}#{sig}" }
    signature = params.empty? ? "" : "(#{render_params(params)})"
    out << "#{indent}def #{m[:self]}#{m[:name]}#{signature}"
    out << "#{indent}end"
  end
  "#{out.join("\n")}\n"
end

File.write(CORE, rewrite(File.read(CORE))) if $PROGRAM_NAME == __FILE__
