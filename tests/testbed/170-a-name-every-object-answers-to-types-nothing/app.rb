class Widget
  def inspect
    "widget"
  end
end

module Describer
  def self.describe(object)
    object.inspect
  end

  def self.keys(hash)
    hash.fetch(:a)
  end

  def self.dump(data)
    data.to_h
  end
end
