# Lists every `def` under ROOT's PATHS as NDJSON: {path, line, owner, name, singleton}.
# The inventory script/dead_compare.py joins each tool's candidates to (by
# path:line) and asks of today's checkout (by owner, name and side).
#
#   ruby script/dead_defs.rb ROOT [PATH...]
#
# Prism only, no bundle: run on any Ruby >= 3.3. The owner is the lexical
# nesting (`module A; class B` -> A::B, `class A::B` -> A::B), which is what
# trekr, Spoom and sorbet-deadcode all report.
require "prism"
require "json"

class DefLister < Prism::Visitor
  def initialize(path, out)
    @path = path
    @out = out
    @nesting = []
    @singleton = false
    super()
  end

  def visit_class_node(node)
    scoped(node.constant_path) { super }
  end

  def visit_module_node(node)
    scoped(node.constant_path) { super }
  end

  def visit_singleton_class_node(node)
    was = @singleton
    @singleton = node.expression.is_a?(Prism::SelfNode)
    super
  ensure
    @singleton = was
  end

  def visit_def_node(node)
    singleton = @singleton || node.receiver.is_a?(Prism::SelfNode)
    @out << { path: @path, line: node.location.start_line, owner: @nesting.join("::"),
              name: node.name.to_s, singleton: singleton }
    # A def nested in a def is not the class's method; neither tool reads it so.
  end

  private

  def scoped(path)
    name = path.slice.delete_prefix("::")
    pushed = name.split("::")
    @nesting.concat(pushed)
    was = @singleton
    @singleton = false
    yield
  ensure
    @singleton = was
    pushed&.size&.times { @nesting.pop }
  end
end

root = ARGV.shift or abort "usage: dead_defs.rb ROOT [PATH...]"
paths = ARGV.empty? ? ["."] : ARGV
skip = %r{(^|/)(vendor|node_modules|tmp|\.git)/}
paths.each do |p|
  Dir.glob(File.join(root, p, "**", "*.rb")).sort.each do |file|
    rel = file.delete_prefix("#{root}/").delete_prefix("./")
    next if rel.match?(skip)

    out = []
    begin
      Prism.parse_file(file).value.accept(DefLister.new(rel, out))
    rescue StandardError
      next
    end
    out.each { |row| puts JSON.generate(row) }
  end
end
