class Node
  @registry = []

  def self.inherited(klass)
    @registry << klass
  end
end
