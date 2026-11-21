class User
  def self.check(s)
    Status.values.include?(s) && Status.deserialize(s)
  end

  def self.pick
    Color::Red
  end
end
User.check(1)
User.pick
Box.new
