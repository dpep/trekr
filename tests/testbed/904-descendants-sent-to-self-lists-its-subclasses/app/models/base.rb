class Base
  def self.registry
    descendants.map(&:name)
  end
end
