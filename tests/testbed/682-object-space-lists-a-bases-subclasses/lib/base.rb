class Base
  def self.all
    ObjectSpace.each_object(Class).select { |k| k < Base }
  end
end
