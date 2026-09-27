class Relation
  def where(*conditions); end
  def find_each(**options); end
end

module Querying
  METHODS = [:where, :find_each].freeze
  delegate(*METHODS, to: :all)
end

class Widget
  extend Querying
end

class Job
  def run(scope)
    scope.where(name: "a")
    scope.find_each(batch_size: 1)
    Widget.where(name: "a")
  end
end
