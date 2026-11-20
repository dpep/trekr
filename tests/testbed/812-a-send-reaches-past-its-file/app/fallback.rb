class Fallback
  def blocked(k)
    m = "handle_#{k}"
    [1].each { m = "fallback_t" }
    TapA.new.public_send(m)
  end

  def looped(params, items)
    m = params[:x]
    for i in items
      m = "handle_#{i}"
    end
    TapB.new.public_send(m)
  end

  def anded(params, k)
    m = "handle_#{k}"
    m &&= params[:x]
    TapC.new.public_send(m)
  end

  def rescued(params, k)
    m = params[:x]
    begin
      m = "handle_#{k}"
    rescue StandardError
      nil
    end
    TapD.new.public_send(m)
  end

  def nilled(k)
    m = nil
    [1].each do
      m = "handle_#{k}"
      TapE.new.public_send(m)
    end
  end
end
f = Fallback.new
f.blocked(1)
f.looped({}, [])
f.anded({}, 1)
f.rescued({}, 1)
f.nilled(1)
