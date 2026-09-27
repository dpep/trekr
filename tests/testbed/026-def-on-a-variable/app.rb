class Greeter
  def name
    "method"
  end

  def run(count)
    x = 5
    puts x
    name = "local"
    puts name
    puts name()
    puts count
    @seen = x
    @seen
  end
end
