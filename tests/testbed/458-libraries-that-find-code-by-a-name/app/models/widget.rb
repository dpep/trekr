class Widget
  def to_drop
    "#{self.class.name}Drop".constantize.new(self)
  end
end
