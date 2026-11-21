class Pair
  def left=(v)
    @l = v
  end

  def right=(v)
    @r = v
  end

  def err=(e)
    @e = e
  end

  def lonely=(v)
    @x = v
  end
end

class User
  def go(list)
    pr = Pair.new
    pr.left, pr.right = 1, 2
    begin
      list.fetch(0)
    rescue => pr.err
    end
  end
end
User.new.go([])
