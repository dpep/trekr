class Registry
  def self.all
    ObjectSpace.each_object(Class).to_a
  end
end
