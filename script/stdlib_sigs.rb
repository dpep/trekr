#!/usr/bin/env ruby
# frozen_string_literal: true

# Write the stdlib's stubs from the signatures the rbs gem ships for it
# (DEC-220):
#
#   ruby script/stdlib_sigs.rb   # rewrites src/tree/stdlib.rb and stdlib_sigs.rb
#
# The stdlib's Ruby is indexed as it is written (DEC-180); this adds what that
# source cannot say. Two kinds of `def`, both read with core_sigs.rb's rules
# (one `sig` per call shape only where every covering overload agrees; no
# `sig` for a union, an optional, `bool`, `self` or an element type):
#
# - a method written in Ruby gets a `def` in stdlib_sigs.rb only when RBS
#   types it. The real `def` stays the location; this one lends its `sig`, as
#   core's stub does for `Set` (DEC-182);
# - a method compiled into a library (`Pathname#read`, `Digest::Class.hexdigest`)
#   gets one in stdlib.rb whether or not RBS types it: it has no source, so
#   the stub is where it is declared.
#
# Which methods exist, who owns them and which are compiled is asked of the
# Ruby itself, without rubygems, so a newer default gem installed as a gem
# cannot stand in for the stdlib's copy. Needs Ruby 3.4 and the rbs it bundles
# (3.8.x), the pair core_sigs.rb reads.

require_relative "core_sigs"
require "json"
require "open3"
require "prism"
require "rbconfig"

abort "stdlib_sigs.rb reads Ruby 3.4's stdlib; this is #{RUBY_VERSION}" unless RUBY_VERSION.start_with?("3.4.")

ROOT = File.expand_path("..", __dir__)
LIBDIR = RbConfig::CONFIG["rubylibdir"]
RBS_DIR = File.join(Gem.loaded_specs.fetch("rbs").gem_dir, "stdlib")

# --- which libraries -----------------------------------------------------------

# What trekr leaves out of the stdlib (`gems::stdlib::SKIPPED`/`KEPT`), read
# from the source so the two cannot disagree.
def trekr_list(name)
  source = File.read(File.join(ROOT, "src/gems/stdlib.rs"))
  body = source[/const #{name}: &\[&str\] = &\[(.*?)\];/m, 1] or abort "no #{name} in stdlib.rs"
  body.scan(/"([^"]+)"/).flatten
end

SKIPPED = trekr_list("SKIPPED")
KEPT = trekr_list("KEPT")

def indexed?(relative)
  return true if KEPT.include?(relative)

  SKIPPED.none? do |skip|
    skip.end_with?("/") ? relative.start_with?(skip) || relative == skip.chomp("/") : relative == skip
  end
end

# The file `require` loads for an rbs library, relative to the stdlib root.
def feature(library)
  [library, library.tr("-", "/")].uniq.find do |name|
    File.exist?(File.join(LIBDIR, "#{name}.rb"))
  end
end

# An rbs library is read when trekr indexes the Ruby it describes. A library
# with no Ruby at all (`stringio`, `zlib`, `etc`) is not in the index, and
# neither are its classes: nothing would own the stubs.
LIBRARIES = Dir.children(RBS_DIR).sort.filter_map do |library|
  path = feature(library) or next
  [library, path] if indexed?("#{path}.rb")
end.to_h

INDEXED = Dir.glob("**/*.rb", base: LIBDIR).select { |path| indexed?(path) }.map { |path| File.join(LIBDIR, path) }

# Every name the indexed stdlib's Ruby defines a method by.
STDLIB_DEFS = INDEXED.flat_map { |path| File.read(path).scan(/^\s*def\s+(?:self\.)?([^\s(;]+)/).flatten }.to_set

# Every class and module the indexed stdlib's Ruby declares with `class` or
# `module`. One it makes at runtime (`OpenSSL::Digest::SHA1`, from
# `Class.new`) is declared by the stub, and so are its methods: the index
# cannot attribute their `def`s to it.
def declared_in(node, nesting, found)
  case node
  when Prism::ClassNode, Prism::ModuleNode
    path = node.constant_path.full_name rescue nil
    if path
      name = path.start_with?("::") ? path.delete_prefix("::") : [*nesting, path].join("::")
      found << name
      node.body && declared_in(node.body, [name], found)
      return found
    end
  end
  node.compact_child_nodes.each { |child| declared_in(child, nesting, found) }
  found
end

STATIC = INDEXED.each_with_object(Set.new) { |path, found| declared_in(Prism.parse_file(path).value, [], found) }

# --- what the Ruby says ------------------------------------------------------

# Run without rubygems, so `require "digest"` is the stdlib's, not a newer
# digest gem's. Answers, for each asked-about method, who owns it at runtime,
# where its source is and its parameters; and for each class, its kind,
# superclass, mixins and whether a named subclass exists.
PROBE = <<~'RUBY'
  require "json"
  ask = JSON.parse($stdin.read)
  require "rbconfig"
  failed = ask["features"].reject do |f|
    require f
    true
  rescue LoadError, StandardError
    false
  end
  const = ->(name) { Object.const_get(name) rescue nil }
  named = ObjectSpace.each_object(Module).select { |m| m.name rescue nil }
  subclassed = named.grep(Class).map(&:superclass).compact.map(&:name).to_h { |n| [n, true] }
  classes = ask["classes"].to_h do |name|
    mod = const.(name)
    next [name, nil] unless mod.is_a?(Module) && mod.name == name
    info = { "kind" => mod.is_a?(Class) ? "class" : "module", "subclassed" => subclassed.key?(name) }
    if mod.is_a?(Class)
      info["superclass"] = mod.superclass&.name
      inherited = mod.superclass ? mod.superclass.ancestors : []
      info["includes"] = (mod.included_modules - inherited).filter_map(&:name).reverse
    else
      info["includes"] = mod.included_modules.filter_map(&:name).reverse
    end
    [name, info]
  end
  methods = ask["methods"].map do |owner, singleton, name|
    mod = const.(owner)
    next nil unless mod.is_a?(Module)
    target = singleton ? mod.singleton_class : mod
    method = target.instance_method(name) rescue nil
    next nil unless method && method.owner == target
    visibility =
      if target.private_method_defined?(name, false) then "private"
      elsif target.protected_method_defined?(name, false) then "protected"
      else "public"
      end
    { "source" => method.source_location&.first, "parameters" => method.parameters, "visibility" => visibility }
  end
  puts JSON.generate({ "classes" => classes, "methods" => methods, "failed" => failed })
RUBY

def probe(classes, methods)
  ruby = RbConfig.ruby
  input = JSON.generate("features" => LIBRARIES.values, "classes" => classes, "methods" => methods)
  out, err, status = Open3.capture3(ruby, "--disable-gems", "-e", PROBE, stdin_data: input)
  abort "probe failed: #{err}" unless status.success?
  JSON.parse(out).tap do |answer|
    abort "probe could not require #{answer['failed'].join(', ')}" unless answer["failed"].empty?
  end
end

# --- what RBS says -----------------------------------------------------------

loader = RBS::EnvironmentLoader.new
LIBRARIES.each_key { |library| loader.add(library:, version: nil) }
ENV_LIB = RBS::Environment.from_loader(loader).resolve_type_names
BUILDER_LIB = RBS::DefinitionBuilder.new(env: ENV_LIB)

def library_of(location)
  path = location&.buffer&.name.to_s
  path[%r{/stdlib/([^/]+)/}, 1]
end

def build(name, singleton)
  type = RBS::TypeName.parse("::#{name}")
  singleton ? BUILDER_LIB.build_singleton(type) : BUILDER_LIB.build_instance(type)
rescue StandardError
  nil
end

# Every method RBS writes into a class — not one it inherits — with the
# library it is written in (nil for core's own RBS).
def rbs_methods
  ENV_LIB.class_decls.keys.flat_map do |type|
    name = type.to_s.delete_prefix("::")
    [false, true].flat_map do |singleton|
      definition = build(name, singleton) or next []
      definition.methods.filter_map do |method_name, method|
        written = method.defs.map { |d| d.member.location }.compact
        next if written.empty?
        # Written in this class's own declarations, not an ancestor's. RBS
        # writes a few modules (`Random::Formatter`) as unnamed ones.
        next unless method.defs.any? { |d| d.defined_in.to_s.delete_prefix("::") == name || d.defined_in.to_s.include?("Unnamed") }

        [name, singleton, method_name.to_s, method, library_of(written.first)]
      end
    end
  end
end

# --- core.rb, which this must not repeat --------------------------------------

def core_methods(lines)
  owners = []
  lines.each_with_object(Set.new) do |line, found|
    if (m = line.match(/\A(\s*)(class|module) ([A-Z][\w:]*)/)) && !line.include?("; end")
      owners.push([m[1].size, [owners.last&.last, m[3]].compact.join("::")])
    elsif line.match?(/\A\s*end\s*\z/) && owners.last && owners.last.first == line[/\A\s*/].size
      owners.pop
    elsif (m = line.match(DEF)) && owners.any?
      found << [owners.last.last, !m[:self].nil?, m[:name]]
    end
  end
end

# --- the stub --------------------------------------------------------------

# Parameters as the Ruby reports them. A compiled method's are unnamed, so
# they are named by position; a Ruby method's are its own.
def runtime_params(parameters)
  parameters.each_with_index.filter_map do |(kind, name), i|
    name = name.to_s
    name = nil if name.empty? || !name.match?(IDENT) || KEYWORDS.include?(name)
    case kind
    when "req" then { kind: :req, name: name || "arg#{i + 1}" }
    when "opt" then { kind: :opt, name: name || "arg#{i + 1}" }
    when "rest" then { kind: :rest, name: name || "args" }
    when "keyreq" then { kind: :keyreq, name: name || "key#{i + 1}" }
    when "key" then { kind: :key, name: name || "key#{i + 1}" }
    when "keyrest" then { kind: :keyrest, name: name || "options" }
    when "block" then { kind: :block, name: name || "block" }
    end
  end
end

# A written return names the constant from the top, since a stub's nesting
# (`module Psych`) would otherwise find `Psych::Set` for `Set`.
def rooted(sig)
  sig.gsub(/returns\(([A-Z][\w:]*)\)/) { "returns(::#{Regexp.last_match(1)})" }
end

# A keyword RBS names that a parameter cannot (`scrypt`'s `N:`) is taken by
# a `**options` instead.
def spellable(params)
  bad, good = params.partition { |p| %i[key keyreq].include?(p[:kind]) && !p[:name].match?(IDENT) }
  return good if bad.empty? || good.any? { |p| p[:kind] == :keyrest }

  at = good.index { |p| p[:kind] == :block } || good.size
  good.insert(at, { kind: :keyrest, name: "options" })
end

Entry = Struct.new(:owner, :singleton, :name, :params, :sigs, :visibility, :compiled, keyword_init: true)

core_lines = File.read(CORE).lines(chomp: true)
core_known = returnable(core_lines)
in_core = core_methods(core_lines)

# A library's dependency (`stringio` for psych) is loaded to resolve its
# types, and is not itself indexed. `initialize` is every class's own, and
# core's variadic one is the honest stub (core_sigs.rb's VARIADIC).
candidates = rbs_methods.reject do |_, _, name, _, library|
  (library && !LIBRARIES.key?(library)) || name == "initialize"
end
# Every class a library's RBS declares, whether or not it writes a method of
# its own: `Digest::SHA256` inherits all of them, and is compiled.
LIBRARY_CLASSES = ENV_LIB.class_decls.filter_map do |type, entry|
  libraries = entry.decls.map { |d| library_of(d.decl.location) }
  type.to_s.delete_prefix("::") if libraries.all? { |library| LIBRARIES.key?(library) }
end

# Every owner, and every namespace around one, so each can be nested.
owners = (candidates.map(&:first) + LIBRARY_CLASSES).flat_map do |owner|
  parts = owner.split("::")
  parts.each_index.map { |i| parts[0..i].join("::") }
end.uniq.sort
answers = probe(owners, candidates.map { |owner, singleton, name, _, _| [owner, singleton, name] })
classes = answers["classes"]

# A return is a class the tree can resolve and nothing subclasses: a Generic
# that is really an HTTP would look `request_uri` up in the wrong class, as
# `Numeric` would `to_s` (DEC-077). A module is never what a call returns,
# only something it includes. Core's classes are core.rb's to judge: a
# library reopening one (`Class`, for `DelegateClass`) does not make it one.
core_declared = core_lines.filter_map { |line| line[/^\s*(?:class|module) ([A-Z]\w*)/, 1] }.to_set
core_modules = core_lines.filter_map { |line| line[/^\s*module ([A-Z]\w*)/, 1] }.to_set
stdlib_known = classes.select { |_, info| info && info["kind"] == "class" && !info["subclassed"] }.keys.to_set
known = (core_known - core_modules) | (stdlib_known - core_declared)

entries = candidates.zip(answers["methods"]).filter_map do |(owner, singleton, name, method, library), runtime|
  next unless runtime

  source = runtime["source"]
  compiled = source.nil?
  # A compiled method's parameters are unnamed: RBS and its call-seq name them.
  existing = runtime_params(runtime["parameters"])
  params = compiled ? spellable(params_for(method, name, existing)) : existing
  # A block's `sig` names the block parameter, which a method that `yield`s
  # does not write.
  if method.method_types.any?(&:block) && params.none? { |p| p[:kind] == :block }
    params += [{ kind: :block, name: "block" }]
  end
  if compiled && library.nil?
    # Core's own compiled methods are core.rb's to declare. One the stdlib
    # also writes in Ruby (`time.rb`'s `xmlschema`, which 3.4 compiled) is
    # indexed there, and takes its return from here. A Ruby method core.rb
    # stubs (`Time.parse`) is lent one the same way, below.
    next unless STDLIB_DEFS.include?(name) && !in_core.include?([owner, singleton, name])

    compiled = false
  elsif compiled
    next if in_core.include?([owner, singleton, name])
  else
    next unless source.start_with?("#{LIBDIR}/") && indexed?(source.delete_prefix("#{LIBDIR}/"))

    compiled = !STATIC.include?(owner)
  end

  sigs = sigs_for(method, known, params).map { |sig| rooted(sig) }
  next if sigs.empty? && !compiled

  Entry.new(owner:, singleton:, name:, params:, sigs:, visibility: runtime["visibility"], compiled:)
end

# --- writing it ----------------------------------------------------------------

GENERATED = "Generated by `script/stdlib_sigs.rb` from rbs #{Gem.loaded_specs.fetch('rbs').version} and Ruby " \
            "#{RUBY_VERSION}'s\n# stdlib; do not edit by hand (DEC-220)."

COMPILED_HEADER = <<~RUBY
  # Ruby's standard library, the half its Ruby does not write: methods compiled
  # into an extension (`Pathname#read`, `Digest::Class.hexdigest`), as RBS
  # describes them. #{GENERATED}
  #
  # Served only when a checkout's stdlib is indexed, one file per top-level
  # owner, and every method is a declaration. A class is declared here only
  # when no Ruby file declares it (`Digest::SHA256`); only then do its
  # superclass and mixins here count.
RUBY

WRITTEN_HEADER = <<~RUBY
  # Return types for stdlib methods written in Ruby, which the stdlib's own
  # source does not say. #{GENERATED}
  #
  # Never a location: each `def` lends its `sig` to the real one in the
  # stdlib, as core's stub does for `Set` (DEC-182).
RUBY

def emit_owner(out, fqn, entries, classes, depth)
  indent = "  " * depth
  info = classes[fqn]
  line = "#{indent}#{info['kind']} #{fqn.split('::').last}"
  line += " < ::#{info['superclass']}" if info["kind"] == "class" && info["superclass"] && info["superclass"] != "Object"
  out << line
  body = info["includes"].map { |mod| "#{indent}  include ::#{mod}" }
  # Class methods first: a `private` above them would read as theirs.
  [true, false].each do |singleton|
    group = entries.select { |e| e.singleton == singleton }.sort_by(&:name)
    %w[public protected private].each do |visibility|
      listed = group.select { |e| e.visibility == visibility }
      next if listed.empty?

      body << "" unless body.empty?
      body << "#{indent}  #{visibility}" unless visibility == "public"
      listed.each_with_index do |entry, i|
        body << "" if i.positive?
        entry.sigs.each { |sig| body << "#{indent}  #{sig}" }
        signature = entry.params.empty? ? "" : "(#{render_params(entry.params)})"
        body << "#{indent}  def #{singleton ? 'self.' : ''}#{entry.name}#{signature}"
        body << "#{indent}  end"
      end
    end
  end
  out.concat(body)
end

# The entries as Ruby, each owner nested in the namespaces around it.
def render(entries, classes, header, declared: [])
  by_owner = entries.group_by(&:owner)
  described = (by_owner.keys + declared).flat_map do |fqn|
    parts = fqn.split("::")
    parts.each_index.map { |i| parts[0..i].join("::") }
  end.uniq.select do |fqn|
    parts = fqn.split("::")
    parts.each_index.all? { |i| classes[parts[0..i].join("::")] }
  end.sort

  out = header.lines(chomp: true)
  described.map { |fqn| fqn.split("::").first }.uniq.each do |top|
    out << ""
    nested = described.select { |fqn| fqn == top || fqn.start_with?("#{top}::") }
    stack = []
    nested.each_with_index do |fqn, i|
      out << "" unless out.last.to_s.empty? || out.last.to_s.match?(/\A\s*(class|module) /)
      emit_owner(out, fqn, by_owner.fetch(fqn, []), classes, stack.size)
      stack.push(fqn)
      # Close every owner the next one does not nest in.
      following = nested[i + 1]
      until stack.empty? || following&.start_with?("#{stack.last}::")
        out << "#{'  ' * (stack.size - 1)}end"
        stack.pop
      end
    end
  end
  "#{out.join("\n")}\n"
end

compiled, written = entries.partition(&:compiled)
File.write(File.join(ROOT, "src/tree/stdlib.rb"), render(compiled, classes, COMPILED_HEADER, declared: LIBRARY_CLASSES))
File.write(File.join(ROOT, "src/tree/stdlib_sigs.rb"), render(written, classes, WRITTEN_HEADER))
warn "#{LIBRARIES.size} libraries: #{compiled.size} compiled methods " \
     "(#{compiled.count { |e| e.sigs.any? }} typed), #{written.size} Ruby methods typed"
