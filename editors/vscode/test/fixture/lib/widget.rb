class Widget
  def initialize(name)
    @name = name
  end

  def save
    true
  end

  def self.build(name)
    new(name)
  end
end
