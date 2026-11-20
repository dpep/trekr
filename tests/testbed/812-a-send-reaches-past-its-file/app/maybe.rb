class Maybe
  def looped(params, k)
    n = params[:x]
    n = "handle_#{k}" while false
    PumpA.new.public_send(n)
  end

  def matched(params, k)
    n = params[:x]
    case k
    in Integer then n = "handle_#{k}"
    end
    PumpB.new.public_send(n)
  end

  def blocked(params, list)
    n = params[:x]
    list.each { |x| n = "handle_#{x}" }
    PumpC.new.public_send(n)
  end

  def unknown_after(params, k)
    n = "handle_#{k}"
    n = params[:x] if k
    PumpD.new.public_send(n)
  end
end
m = Maybe.new
m.looped({}, 1)
m.matched({}, 1)
m.blocked({}, [])
m.unknown_after({}, 1)
