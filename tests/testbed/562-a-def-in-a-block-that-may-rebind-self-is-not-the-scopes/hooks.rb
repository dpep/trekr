module Hooks
  def self.on_load(name, &block)
    name
  end
end

class Target
  def name
    :target
  end
end
