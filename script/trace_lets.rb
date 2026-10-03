# Record which of a suite's example group members ran, as a gold set for
# scoring `--dead`'s `let`, `subject` and group `def` rows (DEC-492).
#
#     cd <an app whose suite runs>
#     LET_TRACE_OUT=/tmp/lets.ndjson bundle exec rspec -r /path/to/trekr/script/trace_lets.rb
#     trekr --dead spec --json > /tmp/dead.json
#     script/dead_lets.py /tmp/lets.ndjson /tmp/dead.json .
#
# A hand label says what a reader thinks runs; this says what did. Every
# `let` and `subject` RSpec defines is wrapped where its block is kept (the
# group's LetDefinitions module), every `def` a group's body adds is wrapped
# by a prepended module, and each records its calls under the definition's
# file and line. A member whose group, or a group nested in it, ran an
# example that passed, and which was never called, is truly unused; one in a
# group with no passing example is unknown, and scored as neither. Shared
# groups are recorded as RSpec registers them, with how often each was
# included.
#
# Environment:
#   LET_TRACE_OUT   output path (required)
#   LET_TRACE_ROOT  the checkout, to make paths relative (default: the cwd)

require "json"
require "set"

module LetTrace
  ROOT = File.expand_path(ENV.fetch("LET_TRACE_ROOT", Dir.pwd)) + "/"
  OUT = ENV.fetch("LET_TRACE_OUT")
  # site => {kind:, name:, path:, line:, groups: Set, hits: Integer}
  SITES = {}
  RAN = Set.new # group classes with a passed example (and their parents)
  SHARED = {} # definition site => {name:, path:, line:, included: Integer}

  def self.rel(path)
    path.start_with?(ROOT) ? path.delete_prefix(ROOT) : nil
  end

  def self.site(kind, name, loc, group)
    path = loc && rel(loc[0])
    return unless path
    key = "#{kind}|#{name}|#{path}:#{loc[1]}"
    entry = (SITES[key] ||= { kind: kind, name: name.to_s, path: path, line: loc[1], groups: Set.new, hits: 0 })
    entry[:groups] << group
    entry
  end

  module Let
    def let(name, &block)
      super
      entry = LetTrace.site("let", name, block&.source_location, self)
      return unless entry
      mod = RSpec::Core::MemoizedHelpers.module_for(self)
      original = mod.instance_method(name)
      mod.__send__(:remove_method, name)
      mod.__send__(:define_method, name) do |*args, &blk|
        entry[:hits] += 1
        original.bind_call(self, *args, &blk)
      end
    end
  end

  module Added
    def method_added(name)
      super
      return if Thread.current[:let_trace_adding]
      loc = instance_method(name).source_location
      return unless loc && LetTrace.rel(loc[0]) && !loc[0].include?("/gems/")
      entry = LetTrace.site("def", name, loc, self)
      return unless entry
      Thread.current[:let_trace_adding] = true
      wrapper = Module.new do
        define_method(name) do |*args, &blk|
          entry[:hits] += 1
          super(*args, &blk)
        end
        ruby2_keywords(name)
      end
      prepend wrapper
    ensure
      Thread.current[:let_trace_adding] = false
    end
  end

  module Example
    def run(*)
      result = super
      if execution_result.status == :passed
        example_group.parent_groups.each { |g| RAN << g }
      end
      result
    end
  end

  module Registry
    def add(context, name, *metadata_args, &block)
      loc = block&.source_location
      path = loc && LetTrace.rel(loc[0])
      if path
        SHARED["#{path}:#{loc[1]}"] ||= { name: name.to_s, path: path, line: loc[1], top: context == :main, included: 0 }
      end
      super
    end
  end

  module Include
    def include_in(klass, *rest)
      loc = @definition&.source_location
      path = loc && LetTrace.rel(loc[0])
      entry = path && SHARED["#{path}:#{loc[1]}"]
      entry[:included] += 1 if entry
      super
    end
  end

  def self.dump
    File.open(OUT, "w") do |out|
      SITES.each_value do |e|
        ran = e[:groups].any? { |g| RAN.include?(g) }
        out.puts JSON.generate(type: e[:kind], name: e[:name], path: e[:path], line: e[:line],
                               groups: e[:groups].size, ran: ran, hits: e[:hits])
      end
      SHARED.each_value { |e| out.puts JSON.generate(e.merge(type: "shared")) }
    end
  end
end

RSpec::Core::MemoizedHelpers::ClassMethods.prepend(LetTrace::Let)
RSpec::Core::ExampleGroup.singleton_class.prepend(LetTrace::Added)
RSpec::Core::Example.prepend(LetTrace::Example)
RSpec::Core::SharedExampleGroup::Registry.prepend(LetTrace::Registry)
RSpec::Core::SharedExampleGroupModule.prepend(LetTrace::Include)
at_exit { LetTrace.dump }
